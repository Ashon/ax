//! Retry-aware wake scheduler for workspaces with unread messages.
//!
//! Mirrors `internal/daemon/wakescheduler.go`: when a `send_message`
//! or broadcast lands in a workspace inbox the scheduler records a
//! pending wake. A background loop wakes the target tmux session when
//! the agent is idle, retrying with exponential backoff (5s → 10s →
//! 20s → 40s → 60s cap) up to ten attempts. Optional collaborators
//! install queue rehydration, missing-session recovery, and a
//! post-success retry policy so task-flow hooks can plug in without
//! widening this module's surface.
//!
//! The scheduler is generic over a [`WakeBackend`] trait so tests can
//! substitute a fake tmux without shelling out. Production wiring
//! uses [`RealWakeBackend`], which delegates to the [`ax_tmux`] free
//! functions.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Notify;

use crate::daemonutil::wake_prompt;
use crate::queue::MessageQueue;

pub const WAKE_CHECK_INTERVAL: Duration = Duration::from_secs(3);
pub const WAKE_MAX_ATTEMPTS: usize = 10;

/// Exponential backoff table for retries. Matches Go verbatim.
#[must_use]
pub fn wake_backoff(attempt: usize) -> Duration {
    match attempt {
        0 => Duration::from_secs(5),
        1 => Duration::from_secs(10),
        2 => Duration::from_secs(20),
        3 => Duration::from_secs(40),
        _ => Duration::from_secs(60),
    }
}

/// Interface the scheduler uses to inspect and wake tmux sessions.
/// Implementors need to be safe to call from a tokio task; blocking
/// shell-outs are fine since the retry cadence is seconds-scale.
pub trait WakeBackend: Send + Sync + 'static {
    fn session_exists(&self, workspace: &str) -> bool;
    fn is_idle(&self, workspace: &str) -> bool;
    fn wake_workspace(&self, workspace: &str, prompt: &str) -> Result<(), ax_tmux::TmuxError>;
}

/// Production backend that delegates to the [`ax_tmux`] free
/// functions.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealWakeBackend;

impl WakeBackend for RealWakeBackend {
    fn session_exists(&self, workspace: &str) -> bool {
        ax_tmux::session_exists(workspace)
    }

    fn is_idle(&self, workspace: &str) -> bool {
        ax_tmux::is_idle(workspace)
    }

    fn wake_workspace(&self, workspace: &str, prompt: &str) -> Result<(), ax_tmux::TmuxError> {
        ax_tmux::wake_workspace(workspace, prompt)
    }
}

/// Optional policy hook: decides whether to keep retrying after a
/// successful wake. Returning `false` removes the workspace from the
/// retry schedule.
pub(crate) type RetryAfterSuccessful = Box<dyn Fn(&str) -> bool + Send + Sync + 'static>;

/// Optional queue rehydrator: called when a due retry finds the
/// inbox empty. Must return the number of messages it re-enqueued.
pub(crate) type QueueRefiller = Box<dyn Fn(&str) -> usize + Send + Sync + 'static>;

/// Optional missing-session recovery: called when a due retry finds
/// no live tmux session. `true` means the session was recreated and
/// the retry should be counted; `false` cancels the pending wake.
pub(crate) type MissingSessionEnsurer = Box<dyn Fn(&str, &str) -> bool + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub(crate) struct PendingWake {
    pub workspace: String,
    pub sender: String,
    pub attempts: usize,
    pub next_retry: DateTime<Utc>,
}

/// Snapshot copy of a pending wake used by the session manager's
/// idle-sleep guard and by the `list_workspaces` status enrichment.
#[derive(Debug, Clone)]
pub struct WakeState {
    pub workspace: String,
    pub sender: String,
    pub attempts: usize,
    pub next_retry: DateTime<Utc>,
}

struct Inner {
    pending: BTreeMap<String, PendingWake>,
    refill: Option<QueueRefiller>,
    ensure_session: Option<MissingSessionEnsurer>,
    retry_after_successful: Option<RetryAfterSuccessful>,
}

pub struct WakeScheduler<B: WakeBackend = RealWakeBackend> {
    backend: B,
    queue: Arc<MessageQueue>,
    inner: Mutex<Inner>,
    notify: Notify,
}

impl<B: WakeBackend> std::fmt::Debug for WakeScheduler<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pending_len = self.inner.lock().map(|g| g.pending.len()).unwrap_or(0);
        f.debug_struct("WakeScheduler")
            .field("pending", &pending_len)
            .finish_non_exhaustive()
    }
}

impl<B: WakeBackend> WakeScheduler<B> {
    #[must_use]
    pub fn new(queue: Arc<MessageQueue>, backend: B) -> Arc<Self> {
        Arc::new(Self {
            backend,
            queue,
            inner: Mutex::new(Inner {
                pending: BTreeMap::new(),
                refill: None,
                ensure_session: None,
                retry_after_successful: None,
            }),
            notify: Notify::new(),
        })
    }

    pub fn set_queue_refiller(&self, refill: QueueRefiller) {
        self.inner.lock().expect("wake scheduler poisoned").refill = Some(refill);
    }

    pub fn set_missing_session_ensurer(&self, fn_: MissingSessionEnsurer) {
        self.inner
            .lock()
            .expect("wake scheduler poisoned")
            .ensure_session = Some(fn_);
    }

    pub fn set_retry_after_successful_wake(&self, fn_: RetryAfterSuccessful) {
        self.inner
            .lock()
            .expect("wake scheduler poisoned")
            .retry_after_successful = Some(fn_);
    }

    /// Register (or reset) a pending wake for `workspace`. Always
    /// schedules the first retry 5s out, matching Go.
    pub fn schedule(&self, workspace: &str, sender: &str) {
        let next = Utc::now() + chrono::Duration::seconds(5);
        let entry = PendingWake {
            workspace: workspace.to_owned(),
            sender: sender.to_owned(),
            attempts: 0,
            next_retry: next,
        };
        {
            let mut inner = self.inner.lock().expect("wake scheduler poisoned");
            inner.pending.insert(workspace.to_owned(), entry);
        }
        self.notify.notify_one();
    }

    pub fn cancel(&self, workspace: &str) {
        let mut inner = self.inner.lock().expect("wake scheduler poisoned");
        inner.pending.remove(workspace);
    }

    #[must_use]
    pub fn state(&self, workspace: &str) -> Option<WakeState> {
        let inner = self.inner.lock().expect("wake scheduler poisoned");
        inner.pending.get(workspace).map(|p| WakeState {
            workspace: p.workspace.clone(),
            sender: p.sender.clone(),
            attempts: p.attempts,
            next_retry: p.next_retry,
        })
    }

    /// Walk the pending map and fire every entry whose `next_retry`
    /// is now in the past. Exposed for tests so they can drive the
    /// state machine without waiting on a tokio ticker.
    pub fn process(&self) {
        let ready: Vec<PendingWake> = {
            let inner = self.inner.lock().expect("wake scheduler poisoned");
            let now = Utc::now();
            inner
                .pending
                .values()
                .filter(|pw| pw.next_retry <= now)
                .cloned()
                .collect()
        };

        for pw in ready {
            // Step 1: empty inbox → try to refill via the task-store
            // hook, otherwise cancel the retry.
            if self.queue.pending_count(&pw.workspace) == 0 {
                let refilled = {
                    let inner = self.inner.lock().expect("wake scheduler poisoned");
                    inner
                        .refill
                        .as_ref()
                        .map_or(0, |refill| refill(&pw.workspace))
                };
                if refilled > 0 {
                    tracing::info!(
                        workspace = %pw.workspace,
                        rehydrated = refilled,
                        "wake rehydrated runnable task messages"
                    );
                }
            }
            if self.queue.pending_count(&pw.workspace) == 0 {
                self.cancel(&pw.workspace);
                continue;
            }

            // Step 2: session missing → ask the ensurer to recreate
            // it. If it refuses, cancel — we have no live target.
            if !self.backend.session_exists(&pw.workspace) {
                let ensured = {
                    let inner = self.inner.lock().expect("wake scheduler poisoned");
                    inner
                        .ensure_session
                        .as_ref()
                        .is_some_and(|fn_| fn_(&pw.workspace, &pw.sender))
                };
                if !ensured {
                    self.cancel(&pw.workspace);
                    continue;
                }
                self.advance_attempt(&pw.workspace, None);
                continue;
            }

            // Step 3: session exists but agent is mid-turn. Re-queue
            // to the next tick without incrementing attempts.
            if !self.backend.is_idle(&pw.workspace) {
                let mut inner = self.inner.lock().expect("wake scheduler poisoned");
                if let Some(entry) = inner.pending.get_mut(&pw.workspace) {
                    entry.next_retry = Utc::now()
                        + chrono::Duration::from_std(WAKE_CHECK_INTERVAL).expect("interval");
                }
                continue;
            }

            // Step 4: idle and has messages — send keys.
            let prompt = wake_prompt(&pw.sender, false);
            let result = self.backend.wake_workspace(&pw.workspace, &prompt);
            self.advance_attempt(&pw.workspace, Some(result));
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn advance_attempt(
        &self,
        workspace: &str,
        wake_result: Option<Result<(), ax_tmux::TmuxError>>,
    ) {
        let mut inner = self.inner.lock().expect("wake scheduler poisoned");
        // Split-borrow: separate the &mut into disjoint field refs
        // so the retry hook can be consulted while we still hold a
        // mutable borrow on `pending`.
        let Inner {
            pending,
            retry_after_successful,
            ..
        } = &mut *inner;
        let Some(entry) = pending.get_mut(workspace) else {
            return;
        };
        entry.attempts += 1;
        let attempts = entry.attempts;

        let wake_failed = matches!(wake_result, Some(Err(_)));
        let wake_ok = matches!(wake_result, Some(Ok(())));
        let wake_none = wake_result.is_none();
        let keep_retrying = if wake_ok || wake_none {
            retry_after_successful
                .as_ref()
                .is_none_or(|hook| hook(workspace))
        } else {
            true
        };
        let max_reached = attempts >= WAKE_MAX_ATTEMPTS;

        if wake_failed || max_reached || !keep_retrying {
            match wake_result.as_ref() {
                Some(Err(e)) => tracing::warn!(
                    workspace = %workspace,
                    attempt = attempts,
                    error = %e,
                    "wake failed; dropping from schedule"
                ),
                Some(Ok(())) if !keep_retrying => tracing::info!(
                    workspace = %workspace,
                    "wake succeeded and retry policy cleared further nudges"
                ),
                Some(Ok(())) => tracing::warn!(
                    workspace = %workspace,
                    attempts = attempts,
                    "wake max attempts reached"
                ),
                None if !keep_retrying => tracing::info!(
                    workspace = %workspace,
                    "wake ensured a session and retry policy cleared further nudges"
                ),
                None => tracing::warn!(
                    workspace = %workspace,
                    attempts = attempts,
                    "wake max attempts reached (session ensure)"
                ),
            }
            pending.remove(workspace);
            return;
        }

        entry.next_retry = Utc::now()
            + chrono::Duration::from_std(wake_backoff(attempts)).expect("backoff duration");
    }

    /// Start the background loop tied to `shutdown`. The returned
    /// handle completes once the loop exits after the shutdown flag
    /// is raised.
    pub fn spawn(self: Arc<Self>) -> WakeLoopHandle {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_task = stop.clone();
        let join = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(WAKE_CHECK_INTERVAL);
            ticker.tick().await; // skip the immediate first tick
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        self.process();
                    }
                    () = self.notify.notified() => {
                        // Brief delay to let immediate dispatch paths
                        // (MCP request → tool wake) land first.
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        self.process();
                    }
                }
                if stop_task.load(Ordering::Relaxed) {
                    break;
                }
            }
        });
        WakeLoopHandle {
            stop,
            join: Some(join),
        }
    }
}

/// Handle to the wake-scheduler background loop. Drop or await
/// [`shutdown`] to stop it.
#[must_use = "loop runs until shutdown"]
pub struct WakeLoopHandle {
    stop: Arc<AtomicBool>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl WakeLoopHandle {
    pub async fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            join.abort();
            let _ = join.await;
        }
    }
}

impl Drop for WakeLoopHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ax_proto::types::Message;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[derive(Clone)]
    struct FakeBackend {
        exists: Arc<std::sync::Mutex<bool>>,
        idle: Arc<std::sync::Mutex<bool>>,
        wake_result: Arc<std::sync::Mutex<Result<(), String>>>,
        wake_calls: Arc<AtomicUsize>,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                exists: Arc::new(std::sync::Mutex::new(false)),
                idle: Arc::new(std::sync::Mutex::new(false)),
                wake_result: Arc::new(std::sync::Mutex::new(Ok(()))),
                wake_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl FakeBackend {
        fn set_exists(&self, value: bool) {
            *self.exists.lock().unwrap() = value;
        }
        fn set_idle(&self, value: bool) {
            *self.idle.lock().unwrap() = value;
        }
        fn set_wake_error(&self, msg: &str) {
            *self.wake_result.lock().unwrap() = Err(msg.to_owned());
        }
    }

    impl WakeBackend for FakeBackend {
        fn session_exists(&self, _ws: &str) -> bool {
            *self.exists.lock().unwrap()
        }
        fn is_idle(&self, _ws: &str) -> bool {
            *self.idle.lock().unwrap()
        }
        fn wake_workspace(&self, _ws: &str, _prompt: &str) -> Result<(), ax_tmux::TmuxError> {
            self.wake_calls.fetch_add(1, AtomicOrdering::Relaxed);
            match self.wake_result.lock().unwrap().clone() {
                Ok(()) => Ok(()),
                Err(msg) => Err(ax_tmux::TmuxError::Command {
                    op: "wake".into(),
                    message: msg,
                }),
            }
        }
    }

    fn seed_message(queue: &MessageQueue, to: &str) {
        queue.enqueue(Message {
            id: String::new(),
            from: "orch".into(),
            to: to.into(),
            content: "hi".into(),
            task_id: String::new(),
            created_at: chrono::Utc::now(),
        });
    }

    #[test]
    fn backoff_schedule_matches_go() {
        assert_eq!(wake_backoff(0), Duration::from_secs(5));
        assert_eq!(wake_backoff(1), Duration::from_secs(10));
        assert_eq!(wake_backoff(2), Duration::from_secs(20));
        assert_eq!(wake_backoff(3), Duration::from_secs(40));
        assert_eq!(wake_backoff(4), Duration::from_secs(60));
        assert_eq!(wake_backoff(42), Duration::from_secs(60));
    }

    #[test]
    fn process_cancels_when_inbox_is_empty_and_refiller_absent() {
        let queue = MessageQueue::new();
        let backend = FakeBackend::default();
        let sched = WakeScheduler::new(queue, backend);
        sched.schedule("worker", "orch");
        // Force due: rewind next_retry so process() treats it as
        // ready immediately.
        {
            let mut inner = sched.inner.lock().unwrap();
            let entry = inner.pending.get_mut("worker").unwrap();
            entry.next_retry = Utc::now() - chrono::Duration::seconds(1);
        }
        sched.process();
        assert!(sched.state("worker").is_none(), "empty inbox should cancel");
    }

    #[test]
    fn refiller_re_enqueues_before_cancel() {
        let queue = MessageQueue::new();
        let queue_clone = queue.clone();
        let backend = FakeBackend::default();
        backend.set_exists(true);
        backend.set_idle(true);
        let sched = WakeScheduler::new(queue, backend.clone());
        sched.set_queue_refiller(Box::new(move |workspace| {
            seed_message(&queue_clone, workspace);
            1
        }));
        sched.schedule("worker", "orch");
        {
            let mut inner = sched.inner.lock().unwrap();
            let entry = inner.pending.get_mut("worker").unwrap();
            entry.next_retry = Utc::now() - chrono::Duration::seconds(1);
        }
        sched.process();
        // Inbox was refilled then a wake keys call was issued.
        assert_eq!(backend.wake_calls.load(AtomicOrdering::Relaxed), 1);
        let state = sched.state("worker").expect("still scheduled");
        assert_eq!(state.attempts, 1);
    }

    #[test]
    fn wake_error_drops_the_schedule() {
        let queue = MessageQueue::new();
        seed_message(&queue, "worker");
        let backend = FakeBackend::default();
        backend.set_exists(true);
        backend.set_idle(true);
        backend.set_wake_error("send-keys failed");
        let sched = WakeScheduler::new(queue, backend.clone());
        sched.schedule("worker", "orch");
        {
            let mut inner = sched.inner.lock().unwrap();
            let entry = inner.pending.get_mut("worker").unwrap();
            entry.next_retry = Utc::now() - chrono::Duration::seconds(1);
        }
        sched.process();
        assert!(
            sched.state("worker").is_none(),
            "wake error should drop entry"
        );
    }

    #[test]
    fn non_idle_reschedules_without_incrementing_attempts() {
        let queue = MessageQueue::new();
        seed_message(&queue, "worker");
        let backend = FakeBackend::default();
        backend.set_exists(true);
        backend.set_idle(false);
        let sched = WakeScheduler::new(queue, backend.clone());
        sched.schedule("worker", "orch");
        {
            let mut inner = sched.inner.lock().unwrap();
            let entry = inner.pending.get_mut("worker").unwrap();
            entry.next_retry = Utc::now() - chrono::Duration::seconds(1);
        }
        sched.process();
        let state = sched.state("worker").expect("still scheduled");
        assert_eq!(state.attempts, 0);
        assert_eq!(backend.wake_calls.load(AtomicOrdering::Relaxed), 0);
    }

    #[test]
    fn retry_policy_false_cancels_after_successful_wake() {
        let queue = MessageQueue::new();
        seed_message(&queue, "worker");
        let backend = FakeBackend::default();
        backend.set_exists(true);
        backend.set_idle(true);
        let sched = WakeScheduler::new(queue, backend.clone());
        sched.set_retry_after_successful_wake(Box::new(|_workspace| false));
        sched.schedule("worker", "orch");
        {
            let mut inner = sched.inner.lock().unwrap();
            let entry = inner.pending.get_mut("worker").unwrap();
            entry.next_retry = Utc::now() - chrono::Duration::seconds(1);
        }
        sched.process();
        assert!(sched.state("worker").is_none());
    }

    #[test]
    fn missing_session_cancels_without_ensurer() {
        let queue = MessageQueue::new();
        seed_message(&queue, "worker");
        let backend = FakeBackend::default();
        backend.set_exists(false);
        let sched = WakeScheduler::new(queue, backend);
        sched.schedule("worker", "orch");
        {
            let mut inner = sched.inner.lock().unwrap();
            let entry = inner.pending.get_mut("worker").unwrap();
            entry.next_retry = Utc::now() - chrono::Duration::seconds(1);
        }
        sched.process();
        assert!(sched.state("worker").is_none());
    }

    #[test]
    fn ensurer_true_counts_attempt_and_reschedules() {
        let queue = MessageQueue::new();
        seed_message(&queue, "worker");
        let backend = FakeBackend::default();
        backend.set_exists(false);
        let sched = WakeScheduler::new(queue, backend);
        sched.set_missing_session_ensurer(Box::new(|_ws, _sender| true));
        sched.schedule("worker", "orch");
        {
            let mut inner = sched.inner.lock().unwrap();
            let entry = inner.pending.get_mut("worker").unwrap();
            entry.next_retry = Utc::now() - chrono::Duration::seconds(1);
        }
        sched.process();
        let state = sched.state("worker").expect("still scheduled");
        assert_eq!(state.attempts, 1);
    }
}
