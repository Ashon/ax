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

use ax_proto::types::{LifecycleAction, LifecycleTarget, TaskStartMode, TaskStatus};
use ax_workspace::{
    dispatch_runnable_work, restart_named_target, start_named_target, stop_named_target,
    DispatchBackend, DispatchError, LifecycleError, TmuxBackend,
};

use crate::registry::Registry;
use crate::task_store::TaskStore;

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
    task_store: Arc<TaskStore>,
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
        task_store: Arc<TaskStore>,
        tmux: B,
    ) -> Arc<Self> {
        Arc::new(Self {
            socket_path,
            ax_bin,
            registry,
            task_store,
            tmux,
        })
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
}
