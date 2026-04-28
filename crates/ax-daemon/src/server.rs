//! Unix-socket server. Accepts newline-delimited JSON envelopes,
//! dispatches them through the handlers module, and spawns a writer
//! task for each registered connection so push envelopes cannot
//! interleave with synchronous responses on the underlying socket.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use ax_proto::Envelope;

use crate::git_status::GitStatusCache;
use crate::handlers::{handle_envelope, refill_runnable_task_messages, HandlerCtx, HandlerOutput};
use crate::history::{History, DEFAULT_HISTORY_MAX_SIZE};
use crate::memory::Store as MemoryStore;
use crate::queue::{FlusherHandle, MessageQueue};
use crate::registry::Registry;
use crate::session_manager::SessionManager;
use crate::shared_values::SharedValues;
use crate::task_store::TaskStore;
use crate::team_reconfigure::TeamController;
use crate::team_state_store::TeamStateStore;
use crate::wake_scheduler::{RealWakeBackend, WakeLoopHandle, WakeScheduler};
use ax_workspace::RealTmux;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("create socket dir {path:?}: {source}")]
    CreateSocketDir { path: PathBuf, source: io::Error },
    #[error("bind unix socket {path:?}: {source}")]
    Bind { path: PathBuf, source: io::Error },
    #[error("accept connection: {0}")]
    Accept(#[source] io::Error),
    #[error("load persisted state: {0}")]
    LoadState(String),
}

/// Configuration handed to [`Daemon::bind`].
#[derive(Debug, Clone)]
pub struct Daemon {
    pub socket_path: PathBuf,
    pub registry: Arc<Registry>,
    pub queue: Arc<MessageQueue>,
    pub shared_values: Arc<SharedValues>,
    pub memory_store: Arc<MemoryStore>,
    pub task_store: Arc<TaskStore>,
    pub team_controller: Arc<TeamController>,
    pub history: Arc<History>,
    pub wake_scheduler: Arc<WakeScheduler<RealWakeBackend>>,
    pub session_manager: Arc<SessionManager<RealTmux>>,
}

impl Daemon {
    /// Build a daemon that keeps all state in memory. Useful for
    /// tests; production callers should use [`Daemon::with_state_dir`]
    /// so shared values and durable memory survive restarts.
    #[must_use]
    pub fn new(socket_path: PathBuf) -> Self {
        let state_dir = socket_path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let shared_values = SharedValues::in_memory();
        let team_store = TeamStateStore::in_memory();
        let team_controller = TeamController::new(state_dir, team_store, shared_values.clone());
        let queue = MessageQueue::new();
        let wake_scheduler = WakeScheduler::new(queue.clone(), RealWakeBackend);
        let registry = Registry::new();
        let task_store = TaskStore::in_memory();
        let history = History::in_memory(DEFAULT_HISTORY_MAX_SIZE);
        let ax_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ax"));
        let session_manager = Arc::new(
            SessionManager::new(
                socket_path.clone(),
                ax_bin,
                registry.clone(),
                queue.clone(),
                task_store.clone(),
                RealTmux,
            )
            .with_wake_scheduler(wake_scheduler.clone()),
        );
        attach_session_manager(&wake_scheduler, &session_manager);
        attach_task_queue_refiller(&wake_scheduler, &task_store, &queue, &history);
        Self {
            socket_path,
            registry,
            queue,
            shared_values,
            memory_store: MemoryStore::in_memory(),
            task_store,
            team_controller,
            history,
            wake_scheduler,
            session_manager,
        }
    }

    /// Attach `state_dir` as the directory where daemon state files
    /// live — `shared_values.json` and `memories.json` today. Errors
    /// if an existing file can't be parsed.
    pub fn with_state_dir(mut self, state_dir: &Path) -> Result<Self, DaemonError> {
        let shared_path = crate::shared_values::default_path(state_dir);
        self.shared_values =
            SharedValues::load(shared_path).map_err(|e| DaemonError::LoadState(e.to_string()))?;
        self.memory_store =
            MemoryStore::load(state_dir).map_err(|e| DaemonError::LoadState(e.to_string()))?;
        self.task_store =
            TaskStore::load(state_dir).map_err(|e| DaemonError::LoadState(e.to_string()))?;
        let recovered = self
            .task_store
            .recover_stale_in_progress(ax_tmux::session_exists);
        if !recovered.is_empty() {
            tracing::info!(
                count = recovered.len(),
                tasks = ?recovered,
                "reset in_progress tasks with dead assignee sessions on startup"
            );
        }
        let team_store =
            TeamStateStore::load(state_dir).map_err(|e| DaemonError::LoadState(e.to_string()))?;
        self.team_controller = TeamController::new(
            state_dir.to_path_buf(),
            team_store,
            self.shared_values.clone(),
        );
        self.queue =
            MessageQueue::load(state_dir).map_err(|e| DaemonError::LoadState(e.to_string()))?;
        self.history = History::load(state_dir, DEFAULT_HISTORY_MAX_SIZE)
            .map_err(|e| DaemonError::LoadState(e.to_string()))?;
        self.wake_scheduler = WakeScheduler::new(self.queue.clone(), RealWakeBackend);
        let ax_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ax"));
        self.session_manager = Arc::new(
            SessionManager::new(
                self.socket_path.clone(),
                ax_bin,
                self.registry.clone(),
                self.queue.clone(),
                self.task_store.clone(),
                RealTmux,
            )
            .with_wake_scheduler(self.wake_scheduler.clone()),
        );
        attach_session_manager(&self.wake_scheduler, &self.session_manager);
        attach_task_queue_refiller(
            &self.wake_scheduler,
            &self.task_store,
            &self.queue,
            &self.history,
        );
        Ok(self)
    }

    /// Bind the Unix socket and spawn the accept loop on the current
    /// tokio runtime. The returned [`DaemonHandle`] stops the server
    /// when dropped via the `shutdown` channel.
    pub async fn bind(self) -> Result<DaemonHandle, DaemonError> {
        if let Some(parent) = self.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|source| {
                DaemonError::CreateSocketDir {
                    path: parent.to_owned(),
                    source,
                }
            })?;
        }
        // A stale socket file left behind from a prior run would make
        // `bind` fail with EADDRINUSE; best-effort remove it first.
        let _ = tokio::fs::remove_file(&self.socket_path).await;

        let listener =
            UnixListener::bind(&self.socket_path).map_err(|source| DaemonError::Bind {
                path: self.socket_path.clone(),
                source,
            })?;

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let socket_path = self.socket_path.clone();
        let registry = self.registry.clone();
        let queue = self.queue.clone();
        let shared = self.shared_values.clone();
        let memory = self.memory_store.clone();
        let task_store = self.task_store.clone();
        let team_controller = self.team_controller.clone();
        let history = self.history.clone();
        let git_status = Arc::new(GitStatusCache::new());
        let wake_scheduler = self.wake_scheduler.clone();
        let session_manager = self.session_manager.clone();
        let flusher = queue.spawn_flusher();
        let wake_loop = wake_scheduler.clone().spawn();
        let idle_loop = spawn_idle_sleep_loop(session_manager.clone());
        let reconcile_loop = spawn_stranded_task_reconciler(
            task_store.clone(),
            queue.clone(),
            wake_scheduler.clone(),
        );
        let join = tokio::spawn(run_accept_loop(
            listener,
            AcceptLoopCtx {
                registry,
                queue,
                shared,
                memory,
                task_store,
                team_controller,
                history,
                git_status,
                wake_scheduler,
                session_manager,
            },
            shutdown_rx,
            socket_path.clone(),
        ));
        Ok(DaemonHandle {
            socket_path,
            shutdown: Some(shutdown_tx),
            join: Some(join),
            flusher: Some(flusher),
            wake_loop: Some(wake_loop),
            idle_loop: Some(idle_loop),
            reconcile_loop: Some(reconcile_loop),
        })
    }
}

const IDLE_SLEEP_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const STRANDED_TASK_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
/// An `InProgress` task whose assignee is idle for this long without
/// sending any update is presumed to have silently exited its turn
/// without calling `update_task`. Short enough that a stuck agent
/// gets nudged within a few minutes; long enough that an agent
/// genuinely in the middle of a multi-step operation isn't
/// interrupted.
const SILENT_TASK_STALE_THRESHOLD_SECS: i64 = 180;
/// Upper bound on how many nudges a single silent task can accrue
/// before the reconciler gives up. Each successful nudge bumps the
/// task's `dispatch_count` via `record_dispatch`, so this doubles as
/// the escalation trigger: if a task needs more nudges than this, a
/// human should intervene.
const SILENT_TASK_NUDGE_CAP: i64 = 5;

/// Body text appended to the silent-exit reminder. Kept as a constant
/// so it can be unit-tested independently of the reconciler loop and
/// so future additions to the task-lifecycle tool surface land in
/// one place. Any MCP agent — Claude, Codex, or future runtimes —
/// sees the same remediation menu.
pub(crate) const SILENT_TASK_NUDGE_NOTE: &str =
    "Assignee looks idle but the task is still `in_progress`. \
     Call one of the task-lifecycle MCP tools instead of leaving it open: \
     `report_task_progress` (heartbeat with a note), \
     `report_task_completion` (done — supply `dirty_files` and optional `residual_scope`), \
     `report_task_failed` (hard error with `reason`), or \
     `report_task_blocked` (need external help — optional `needs_help_from`).";

/// Handle the idle-sleep background task owns. Mirrors the flusher /
/// wake-loop pattern so shutdown is graceful.
pub(crate) struct IdleLoopHandle {
    stop: Arc<std::sync::atomic::AtomicBool>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl IdleLoopHandle {
    async fn shutdown(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            join.abort();
            let _ = join.await;
        }
    }
}

impl Drop for IdleLoopHandle {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

/// Handle the stranded-task reconciler owns. Same shape as
/// `IdleLoopHandle` so `DaemonHandle::shutdown` can drive the
/// identical graceful-stop sequence.
pub(crate) struct ReconcileLoopHandle {
    stop: Arc<std::sync::atomic::AtomicBool>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl ReconcileLoopHandle {
    async fn shutdown(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            join.abort();
            let _ = join.await;
        }
    }
}

impl Drop for ReconcileLoopHandle {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

fn spawn_idle_sleep_loop(session_manager: Arc<SessionManager<RealTmux>>) -> IdleLoopHandle {
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = Arc::new(AtomicBool::new(false));
    let stop_task = stop.clone();
    let join = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(IDLE_SLEEP_CHECK_INTERVAL);
        ticker.tick().await; // skip the immediate first tick
        loop {
            ticker.tick().await;
            if stop_task.load(Ordering::Relaxed) {
                break;
            }
            let _ = session_manager.stop_idle(chrono::Utc::now());
        }
    });
    IdleLoopHandle {
        stop,
        join: Some(join),
    }
}

/// Two sweeps on one ticker:
///
/// 1. **Dead-session recovery** — `recover_stale_in_progress` resets
///    any `InProgress` task whose assignee tmux session is gone.
///    This is the in-flight counterpart to `Daemon::with_state_dir`'s
///    startup sweep; without it, a crashed agent strands its task
///    until the daemon is bounced.
///
/// 2. **Silent-exit nudge** — catches the subtler case where the
///    assignee *is* alive and idle but never called `update_task`.
///    The wake scheduler can't help because its trigger is an inbox
///    message; with no inbox traffic an idle agent that forgot to
///    close its task just sits there. The reconciler enqueues a
///    reminder message, wakes the session, and bumps
///    `last_dispatch_at` via `record_dispatch`. Subsequent ticks skip
///    the task until it goes stale again, and once its
///    `dispatch_count` hits `SILENT_TASK_NUDGE_CAP` the reconciler
///    hands off to a human.
fn spawn_stranded_task_reconciler(
    task_store: Arc<TaskStore>,
    queue: Arc<MessageQueue>,
    wake_scheduler: Arc<WakeScheduler<RealWakeBackend>>,
) -> ReconcileLoopHandle {
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = Arc::new(AtomicBool::new(false));
    let stop_task = stop.clone();
    let join = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(STRANDED_TASK_CHECK_INTERVAL);
        ticker.tick().await; // skip the immediate first tick
        loop {
            ticker.tick().await;
            if stop_task.load(Ordering::Relaxed) {
                break;
            }
            let store = task_store.clone();
            let q = queue.clone();
            let ws = wake_scheduler.clone();
            // tmux liveness/idle checks shell out, so hop onto a
            // blocking thread. Both passes share this one offload.
            tokio::task::spawn_blocking(move || reconcile_once(&store, &q, &ws))
                .await
                .ok();
        }
    });
    ReconcileLoopHandle {
        stop,
        join: Some(join),
    }
}

fn reconcile_once(
    task_store: &TaskStore,
    queue: &MessageQueue,
    wake_scheduler: &WakeScheduler<RealWakeBackend>,
) {
    let recovered = task_store.recover_stale_in_progress(ax_tmux::session_exists);
    if !recovered.is_empty() {
        tracing::info!(
            count = recovered.len(),
            tasks = ?recovered,
            "reconciled stranded in_progress tasks with dead assignee sessions"
        );
    }

    let now = chrono::Utc::now();
    let threshold = chrono::Duration::seconds(SILENT_TASK_STALE_THRESHOLD_SECS);
    for task in task_store.list_silent_in_progress(now, threshold) {
        if task.dispatch_count >= SILENT_TASK_NUDGE_CAP {
            // Escalation boundary: further nudges are just noise.
            // Leaving the task as-is lets a human notice via
            // `list_tasks` / TUI filters.
            continue;
        }
        if !ax_tmux::session_exists(&task.assignee) {
            continue; // handled by the recovery pass above
        }
        if !ax_tmux::is_idle(&task.assignee) {
            continue;
        }
        if queue.pending_count(&task.assignee) > 0 {
            continue; // wake scheduler will handle the existing traffic
        }

        let body = ax_daemon_helpers::build_task_reminder_message(&task, SILENT_TASK_NUDGE_NOTE);
        let msg = ax_daemon_helpers::task_aware_message("reconciler", &task.assignee, &body);
        queue.enqueue(msg);
        let _ = task_store.record_dispatch(&task.id, &task.assignee, now);
        wake_scheduler.schedule(&task.assignee, "reconciler");
        tracing::info!(
            task_id = %task.id,
            assignee = %task.assignee,
            dispatch_count = task.dispatch_count + 1,
            "nudged silent in_progress task"
        );
    }
}

// Thin alias so the reconciler doesn't depend on the task_helpers
// module path shape — the helpers are `pub(crate)` and calling them
// directly keeps the boundary explicit.
mod ax_daemon_helpers {
    pub(super) use crate::task_helpers::{build_task_reminder_message, task_aware_message};
}

/// Hook the session manager into the wake scheduler's missing-session
/// ensurer so retry paths can recreate managed workspaces from a
/// runnable task's stored dispatch config. Extracted so both
/// `Daemon::new` and `Daemon::with_state_dir` can re-use the wiring.
fn attach_session_manager(
    wake_scheduler: &Arc<WakeScheduler<RealWakeBackend>>,
    session_manager: &Arc<SessionManager<RealTmux>>,
) {
    let sm = session_manager.clone();
    wake_scheduler.set_missing_session_ensurer(Box::new(move |workspace, sender| {
        sm.ensure_pending_wake_target(workspace, sender)
    }));
}

fn attach_task_queue_refiller(
    wake_scheduler: &Arc<WakeScheduler<RealWakeBackend>>,
    task_store: &Arc<TaskStore>,
    queue: &Arc<MessageQueue>,
    history: &Arc<History>,
) {
    let task_store = task_store.clone();
    let queue = queue.clone();
    let history = history.clone();
    wake_scheduler.set_queue_refiller(Box::new(move |workspace, sender| {
        refill_runnable_task_messages(
            task_store.as_ref(),
            queue.as_ref(),
            history.as_ref(),
            workspace,
            sender,
        )
    }));
}

struct AcceptLoopCtx {
    registry: Arc<Registry>,
    queue: Arc<MessageQueue>,
    shared: Arc<SharedValues>,
    memory: Arc<MemoryStore>,
    task_store: Arc<TaskStore>,
    team_controller: Arc<TeamController>,
    history: Arc<History>,
    git_status: Arc<GitStatusCache>,
    wake_scheduler: Arc<WakeScheduler<RealWakeBackend>>,
    session_manager: Arc<SessionManager<RealTmux>>,
}

async fn run_accept_loop(
    listener: UnixListener,
    loop_ctx: AcceptLoopCtx,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
    socket_path: PathBuf,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accept = listener.accept() => match accept {
                Ok((conn, _)) => {
                    let ctx = HandlerCtx {
                        socket_path: socket_path.clone(),
                        registry: loop_ctx.registry.clone(),
                        queue: loop_ctx.queue.clone(),
                        shared: loop_ctx.shared.clone(),
                        memory: loop_ctx.memory.clone(),
                        task_store: loop_ctx.task_store.clone(),
                        team_controller: loop_ctx.team_controller.clone(),
                        history: loop_ctx.history.clone(),
                        git_status: loop_ctx.git_status.clone(),
                        wake_scheduler: loop_ctx.wake_scheduler.clone(),
                        session_manager: loop_ctx.session_manager.clone(),
                    };
                    tokio::spawn(handle_connection(conn, ctx));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                }
            },
        }
    }
    let _ = tokio::fs::remove_file(&socket_path).await;
}

async fn handle_connection(stream: UnixStream, ctx: HandlerCtx) {
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let (writer_tx, writer_rx) = mpsc::channel::<Envelope>(super::registry::OUTBOX_CAPACITY);
    let writer_join = tokio::spawn(run_writer(write_half, writer_rx));

    let mut workspace = String::new();
    let mut connection_id: Option<u64> = None;
    let mut push_forwarder: Option<tokio::task::JoinHandle<()>> = None;
    let mut line = String::new();

    loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "read line failed");
                break;
            }
        };
        let _ = n;
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let env = match serde_json::from_str::<Envelope>(trimmed) {
            Ok(env) => env,
            Err(e) => {
                tracing::warn!(error = %e, "decode envelope failed");
                continue;
            }
        };

        let output = handle_envelope(&ctx, &env, &mut workspace, &mut connection_id);
        match output {
            HandlerOutput::Response(resp) => {
                if writer_tx.send(resp).await.is_err() {
                    break;
                }
            }
            HandlerOutput::Registered {
                response,
                entry,
                receiver,
                previous_outbox,
            } => {
                // Close any previous registration's outbox first so
                // the old writer task exits before we re-point pushes
                // at the new connection.
                if let Some(prev) = previous_outbox {
                    drop(prev);
                }
                if let Some(handle) = push_forwarder.take() {
                    handle.abort();
                }
                push_forwarder = Some(spawn_push_forwarder(receiver, writer_tx.clone()));
                if writer_tx.send(response).await.is_err() {
                    break;
                }
                // Sanity: align our local connection_id with the new entry.
                connection_id = Some(entry.id);
            }
        }
    }

    if let Some(id) = connection_id {
        ctx.registry.unregister_if(&workspace, id);
    }
    if let Some(handle) = push_forwarder {
        handle.abort();
    }
    drop(writer_tx);
    let _ = writer_join.await;
}

fn spawn_push_forwarder(
    mut rx: mpsc::Receiver<Envelope>,
    writer: mpsc::Sender<Envelope>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(env) = rx.recv().await {
            if writer.send(env).await.is_err() {
                break;
            }
        }
    })
}

async fn run_writer(
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    mut rx: mpsc::Receiver<Envelope>,
) {
    while let Some(env) = rx.recv().await {
        let mut bytes = match serde_json::to_vec(&env) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "marshal envelope failed");
                continue;
            }
        };
        bytes.push(b'\n');
        if let Err(e) = write_half.write_all(&bytes).await {
            tracing::warn!(error = %e, "write envelope failed");
            break;
        }
    }
}

/// Handle to a running daemon. Drop to shut it down and wait for the
/// accept loop to exit. The Unix socket file is removed when the
/// accept loop returns.
pub struct DaemonHandle {
    socket_path: PathBuf,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
    flusher: Option<FlusherHandle>,
    wake_loop: Option<WakeLoopHandle>,
    idle_loop: Option<IdleLoopHandle>,
    reconcile_loop: Option<ReconcileLoopHandle>,
}

impl DaemonHandle {
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Gracefully stop the server and wait for the accept loop. Also
    /// stops the queue flusher and awaits a final snapshot so no
    /// pending enqueue is lost on clean shutdown.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
        if let Some(flusher) = self.flusher.take() {
            flusher.shutdown().await;
        }
        if let Some(wake_loop) = self.wake_loop.take() {
            wake_loop.shutdown().await;
        }
        if let Some(idle_loop) = self.idle_loop.take() {
            idle_loop.shutdown().await;
        }
        if let Some(reconcile_loop) = self.reconcile_loop.take() {
            reconcile_loop.shutdown().await;
        }
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SILENT_TASK_NUDGE_NOTE;

    #[test]
    fn silent_task_nudge_note_advertises_every_lifecycle_tool() {
        // Any MCP agent reading this nudge should see the full
        // remediation menu. If a tool is added/renamed and this
        // assertion stops holding, update the note — the note is
        // the canonical discovery surface for stuck agents.
        assert!(SILENT_TASK_NUDGE_NOTE.contains("report_task_progress"));
        assert!(SILENT_TASK_NUDGE_NOTE.contains("report_task_completion"));
        assert!(SILENT_TASK_NUDGE_NOTE.contains("report_task_failed"));
        assert!(SILENT_TASK_NUDGE_NOTE.contains("report_task_blocked"));
    }

    #[test]
    fn silent_task_nudge_note_describes_why_nudge_fires() {
        // The note must explain the "why" so an agent does not just
        // retry the same tool call blindly — it should understand
        // the reconciler noticed an idle in_progress task.
        assert!(SILENT_TASK_NUDGE_NOTE.contains("idle"));
        assert!(SILENT_TASK_NUDGE_NOTE.contains("in_progress"));
    }

    #[test]
    fn silent_task_nudge_note_is_non_trivial() {
        // Guard against an accidental truncation / empty string in
        // a future refactor: the note backs stale-recovery UX.
        assert!(SILENT_TASK_NUDGE_NOTE.len() > 100);
    }
}
