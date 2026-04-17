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

use crate::handlers::{handle_envelope, HandlerCtx, HandlerOutput};
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
        let ax_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ax"));
        let session_manager = SessionManager::new(
            socket_path.clone(),
            ax_bin,
            registry.clone(),
            task_store.clone(),
            RealTmux,
        );
        attach_session_manager(&wake_scheduler, &session_manager);
        Self {
            socket_path,
            registry,
            queue,
            shared_values,
            memory_store: MemoryStore::in_memory(),
            task_store,
            team_controller,
            history: History::in_memory(DEFAULT_HISTORY_MAX_SIZE),
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
        self.session_manager = SessionManager::new(
            self.socket_path.clone(),
            ax_bin,
            self.registry.clone(),
            self.task_store.clone(),
            RealTmux,
        );
        attach_session_manager(&self.wake_scheduler, &self.session_manager);
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
        let wake_scheduler = self.wake_scheduler.clone();
        let session_manager = self.session_manager.clone();
        let flusher = queue.spawn_flusher();
        let wake_loop = wake_scheduler.clone().spawn();
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
        })
    }
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

struct AcceptLoopCtx {
    registry: Arc<Registry>,
    queue: Arc<MessageQueue>,
    shared: Arc<SharedValues>,
    memory: Arc<MemoryStore>,
    task_store: Arc<TaskStore>,
    team_controller: Arc<TeamController>,
    history: Arc<History>,
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
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}
