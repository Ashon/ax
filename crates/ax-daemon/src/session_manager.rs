//! Thin orchestration layer that sits between the daemon handlers
//! and the workspace tmux backend. Mirrors
//! `internal/daemon/session_manager.go`: wraps
//! `dispatch_runnable_work` and the lifecycle `{start,stop,restart}
//! _named_target` helpers so handlers and the wake-scheduler's
//! missing-session ensurer hook share one entry point and one
//! config-path validation path.
//!
//! `should_sleep` and `stop_idle` are ported but currently only used
//! by the session-manager unit tests and the `intervene_task` +
//! `start_task` handler flow; the idle-sleep loop in Go
//! (`internal/daemon/idle_sleep.go`) will wire in once the tmux side
//! grows `is_idle` observability.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ax_proto::types::{LifecycleAction, LifecycleTarget, TaskStartMode, TaskStatus};
use ax_workspace::{
    dispatch_runnable_work, restart_named_target, start_named_target, stop_named_target,
    DispatchBackend, DispatchError, LifecycleError, TmuxBackend,
};
use chrono::{DateTime, Utc};

use crate::queue::MessageQueue;
use crate::registry::{RegisteredWorkspace, Registry};
use crate::task_store::TaskStore;
use crate::wake_scheduler::{RealWakeBackend, WakeScheduler};

#[derive(Debug, thiserror::Error)]
pub enum SessionManagerError {
    #[error("invalid lifecycle action {0:?}")]
    InvalidAction(String),
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
}

/// Handle to the workspace tmux operations the daemon needs at
/// runtime. Generic over a backend so tests can drive it with a
/// fake tmux; production wiring uses `ax_workspace::RealTmux`.
pub struct SessionManager<B: TmuxBackend + DispatchBackend + Clone + Send + Sync + 'static> {
    socket_path: PathBuf,
    ax_bin: PathBuf,
    registry: Arc<Registry>,
    queue: Arc<MessageQueue>,
    task_store: Arc<TaskStore>,
    wake_scheduler: Option<Arc<WakeScheduler<RealWakeBackend>>>,
    tmux: B,
}

impl<B: TmuxBackend + DispatchBackend + Clone + Send + Sync + 'static> std::fmt::Debug
    for SessionManager<B>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("socket_path", &self.socket_path)
            .field("ax_bin", &self.ax_bin)
            .finish_non_exhaustive()
    }
}

impl<B: TmuxBackend + DispatchBackend + Clone + Send + Sync + 'static> SessionManager<B> {
    #[must_use]
    pub fn new(
        socket_path: PathBuf,
        ax_bin: PathBuf,
        registry: Arc<Registry>,
        queue: Arc<MessageQueue>,
        task_store: Arc<TaskStore>,
        tmux: B,
    ) -> Self {
        Self {
            socket_path,
            ax_bin,
            registry,
            queue,
            task_store,
            wake_scheduler: None,
            tmux,
        }
    }

    /// Record the wake scheduler so `should_sleep` can check whether
    /// a workspace still has a pending wake before allowing it to be
    /// put to sleep. Called once during `Daemon::bind` wiring.
    #[must_use]
    pub fn with_wake_scheduler(mut self, scheduler: Arc<WakeScheduler<RealWakeBackend>>) -> Self {
        self.wake_scheduler = Some(scheduler);
        self
    }

    /// Kick a dispatch-ready workspace so the tmux session exists and
    /// the inbox gets drained. Thin passthrough to
    /// `ax_workspace::dispatch_runnable_work` using the manager's
    /// captured socket + ax binary paths.
    pub fn ensure_runnable(
        &self,
        config_path: &str,
        target: &str,
        sender: &str,
        fresh: bool,
    ) -> Result<(), SessionManagerError> {
        dispatch_runnable_work(
            &self.tmux,
            &self.socket_path,
            Path::new(config_path),
            &self.ax_bin,
            target,
            sender,
            fresh,
        )
        .map_err(Into::into)
    }

    /// Dispatch a lifecycle action (start / stop / restart) against a
    /// named target resolved out of `config_path`.
    pub fn control(
        &self,
        config_path: &str,
        target_name: &str,
        action: &LifecycleAction,
    ) -> Result<LifecycleTarget, SessionManagerError> {
        let cfg = Path::new(config_path);
        let result = match action {
            LifecycleAction::Start => start_named_target(
                &self.tmux,
                &self.socket_path,
                cfg,
                &self.ax_bin,
                target_name,
            ),
            LifecycleAction::Stop => stop_named_target(
                &self.tmux,
                &self.socket_path,
                cfg,
                &self.ax_bin,
                target_name,
            ),
            LifecycleAction::Restart => restart_named_target(
                &self.tmux,
                &self.socket_path,
                cfg,
                &self.ax_bin,
                target_name,
            ),
        };
        result.map_err(Into::into)
    }

    /// True when the assignee still has a pending / in-progress /
    /// blocked task — used by the idle-sleep guard to avoid
    /// stopping a workspace that still owes work.
    pub fn has_open_assigned_tasks(&self, assignee: &str) -> bool {
        self.task_store
            .list(assignee, "", None)
            .into_iter()
            .any(|task| {
                matches!(
                    task.status,
                    TaskStatus::Pending | TaskStatus::InProgress | TaskStatus::Blocked
                )
            })
    }

    /// Hook installed on the wake scheduler's missing-session
    /// ensurer: when a wake retry finds no live tmux session, try to
    /// re-create it from the first runnable task's stored
    /// `dispatch_config_path`. Returns `true` when a target was
    /// ensured; `false` tells the scheduler to drop the pending wake.
    pub fn ensure_pending_wake_target(&self, workspace: &str, sender: &str) -> bool {
        let workspace = workspace.trim();
        if workspace.is_empty() {
            return false;
        }
        if self.tmux.session_exists(workspace) {
            return true;
        }
        let Some((config_path, fresh)) = self.pending_wake_dispatch_config(workspace) else {
            return false;
        };
        match self.ensure_runnable(&config_path, workspace, sender, fresh) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    workspace = %workspace,
                    error = %e,
                    "wake could not ensure runnable target"
                );
                false
            }
        }
    }

    fn pending_wake_dispatch_config(&self, workspace: &str) -> Option<(String, bool)> {
        let runnable = self
            .task_store
            .runnable_by_assignee(workspace, chrono::Utc::now());
        for task in runnable {
            let config_path = task.dispatch_config_path.trim();
            if config_path.is_empty() {
                continue;
            }
            return Some((
                config_path.to_owned(),
                matches!(task.start_mode, TaskStartMode::Fresh),
            ));
        }
        None
    }

    /// Accessor used by handlers that need to probe tmux state
    /// directly (`intervene_task wake/interrupt`). Returns a cloned
    /// backend so the caller can shell out without holding a lock.
    #[must_use]
    pub fn tmux(&self) -> B {
        self.tmux.clone()
    }

    /// Expose the socket path for handler-side logging.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Touch the registry for `name` — handlers do this after
    /// control-plane actions so the `last_active_at` watermark stays
    /// fresh.
    pub fn touch(&self, name: &str) {
        self.registry.touch(name, chrono::Utc::now());
    }

    /// Walk every registered workspace and stop the ones that have
    /// been idle past their configured timeout and have no work
    /// pending. Mirrors Go's `sessionManager.stopIdle`. Returns the
    /// number of workspaces the loop actually put to sleep so the
    /// caller can log visibility into the reconcile cadence.
    pub fn stop_idle(&self, now: DateTime<Utc>) -> usize {
        let mut stopped = 0usize;
        for registered in self.registry.snapshot() {
            if !self.should_sleep(&registered, now) {
                continue;
            }
            match self.control(
                &registered.config_path,
                &registered.info.name,
                &LifecycleAction::Stop,
            ) {
                Ok(_) => {
                    tracing::info!(
                        workspace = %registered.info.name,
                        idle_for_secs = now
                            .signed_duration_since(registered.last_active_at)
                            .num_seconds(),
                        "idle sleep: stopped workspace with no queued work"
                    );
                    stopped += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        workspace = %registered.info.name,
                        error = %e,
                        "idle sleep skip",
                    );
                }
            }
            self.touch(&registered.info.name);
        }
        stopped
    }

    /// Gate used by [`Self::stop_idle`]. Workspace is sleepable iff
    /// all of: it's a user-session (not an always-on orchestrator),
    /// has a `config_path`, has a positive idle timeout, hasn't been
    /// active within that timeout, the tmux session exists and is
    /// idle, the inbox is empty, there's no pending wake, and no
    /// open assigned task remains.
    #[must_use]
    pub fn should_sleep(&self, registered: &RegisteredWorkspace, now: DateTime<Utc>) -> bool {
        let name = registered.info.name.trim();
        if name.is_empty() || is_always_on_target(name) {
            return false;
        }
        if registered.config_path.trim().is_empty() {
            return false;
        }
        if registered.idle_timeout == Duration::ZERO {
            return false;
        }
        let elapsed = now
            .signed_duration_since(registered.last_active_at)
            .to_std()
            .unwrap_or(Duration::ZERO);
        if elapsed < registered.idle_timeout {
            return false;
        }
        if !self.tmux.session_exists(name) || !self.tmux.is_idle(name) {
            return false;
        }
        if self.queue.pending_count(name) > 0 {
            return false;
        }
        if self
            .wake_scheduler
            .as_ref()
            .and_then(|s| s.state(name))
            .is_some()
        {
            return false;
        }
        !self.has_open_assigned_tasks(name)
    }
}

fn is_always_on_target(name: &str) -> bool {
    let trimmed = name.trim();
    trimmed == "orchestrator" || trimmed.ends_with(".orchestrator")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ax_proto::types::{AgentStatus, WorkspaceInfo};
    use ax_tmux::SessionInfo;
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Mutex as StdMutex;

    #[derive(Default, Clone)]
    struct FakeTmux {
        sessions: Arc<StdMutex<HashSet<String>>>,
        idle: Arc<StdMutex<HashSet<String>>>,
        stops: Arc<StdMutex<Vec<String>>>,
    }

    impl FakeTmux {
        fn with_session(name: &str, idle: bool) -> Self {
            let fake = Self::default();
            fake.sessions.lock().unwrap().insert(name.to_owned());
            if idle {
                fake.idle.lock().unwrap().insert(name.to_owned());
            }
            fake
        }
    }

    impl TmuxBackend for FakeTmux {
        fn session_exists(&self, workspace: &str) -> bool {
            self.sessions.lock().unwrap().contains(workspace)
        }
        fn list_sessions(&self) -> Result<Vec<SessionInfo>, ax_tmux::TmuxError> {
            Ok(Vec::new())
        }
        fn is_idle(&self, workspace: &str) -> bool {
            self.idle.lock().unwrap().contains(workspace)
        }
        fn create_session(
            &self,
            _workspace: &str,
            _dir: &str,
            _shell: &str,
            _env: &BTreeMap<String, String>,
        ) -> Result<(), ax_tmux::TmuxError> {
            Ok(())
        }
        fn create_session_with_command(
            &self,
            _workspace: &str,
            _dir: &str,
            _command: &str,
            _env: &BTreeMap<String, String>,
        ) -> Result<(), ax_tmux::TmuxError> {
            Ok(())
        }
        fn create_session_with_args(
            &self,
            _workspace: &str,
            _dir: &str,
            _argv: &[String],
            _env: &BTreeMap<String, String>,
        ) -> Result<(), ax_tmux::TmuxError> {
            Ok(())
        }
        fn destroy_session(&self, workspace: &str) -> Result<(), ax_tmux::TmuxError> {
            self.sessions.lock().unwrap().remove(workspace);
            self.stops.lock().unwrap().push(workspace.to_owned());
            Ok(())
        }
    }

    impl DispatchBackend for FakeTmux {
        fn wake_workspace(&self, _w: &str, _p: &str) -> Result<(), ax_tmux::TmuxError> {
            Ok(())
        }
    }

    fn sample_registered(name: &str) -> RegisteredWorkspace {
        RegisteredWorkspace {
            info: WorkspaceInfo {
                name: name.to_owned(),
                dir: "/tmp/w".into(),
                description: String::new(),
                status: AgentStatus::Online,
                status_text: String::new(),
                connected_at: Some(Utc::now()),
            },
            config_path: "/tmp/config.yaml".into(),
            idle_timeout: Duration::from_secs(30),
            last_active_at: Utc::now() - chrono::Duration::seconds(120),
        }
    }

    fn sample_manager(tmux: FakeTmux) -> SessionManager<FakeTmux> {
        SessionManager::new(
            PathBuf::from("/tmp/daemon.sock"),
            PathBuf::from("/tmp/ax-rs"),
            Registry::new(),
            MessageQueue::new(),
            TaskStore::in_memory(),
            tmux,
        )
    }

    #[test]
    fn should_sleep_requires_idle_session_with_empty_inbox() {
        let mgr = sample_manager(FakeTmux::with_session("worker", true));
        assert!(mgr.should_sleep(&sample_registered("worker"), Utc::now()));
    }

    #[test]
    fn should_sleep_false_when_idle_timeout_is_zero() {
        let mgr = sample_manager(FakeTmux::with_session("worker", true));
        let mut r = sample_registered("worker");
        r.idle_timeout = Duration::ZERO;
        assert!(!mgr.should_sleep(&r, Utc::now()));
    }

    #[test]
    fn should_sleep_false_for_orchestrator_names() {
        let mgr = sample_manager(FakeTmux::with_session("orchestrator", true));
        let mut r = sample_registered("orchestrator");
        r.info.name = "orchestrator".into();
        assert!(!mgr.should_sleep(&r, Utc::now()));
        r.info.name = "team.orchestrator".into();
        assert!(!mgr.should_sleep(&r, Utc::now()));
    }

    #[test]
    fn should_sleep_false_when_session_busy() {
        let mgr = sample_manager(FakeTmux::with_session("worker", false));
        assert!(!mgr.should_sleep(&sample_registered("worker"), Utc::now()));
    }

    #[test]
    fn should_sleep_false_when_queue_has_pending() {
        let tmux = FakeTmux::with_session("worker", true);
        let mgr = sample_manager(tmux);
        mgr.queue.enqueue(ax_proto::types::Message {
            id: String::new(),
            from: "orch".into(),
            to: "worker".into(),
            content: "hi".into(),
            task_id: String::new(),
            created_at: Utc::now(),
        });
        assert!(!mgr.should_sleep(&sample_registered("worker"), Utc::now()));
    }

    #[test]
    fn should_sleep_false_when_not_idle_long_enough() {
        let mgr = sample_manager(FakeTmux::with_session("worker", true));
        let mut r = sample_registered("worker");
        r.last_active_at = Utc::now(); // just touched
        assert!(!mgr.should_sleep(&r, Utc::now()));
    }
}
