//! Dispatch-target startup + wake helpers.

use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use ax_config::Config;

use crate::{
    build_desired_state_with_tree, cleanup_orchestrator_state, ensure_orchestrator,
    DesiredOrchestrator, DesiredState, DesiredWorkspace, Manager, RealTmux, TmuxBackend,
};

#[derive(Debug, Clone, Copy)]
pub struct DispatchOptions {
    pub ready_timeout: Duration,
    pub ready_poll_interval: Duration,
    pub ready_settle_delay: Duration,
    pub ready_fallback_delay: Duration,
}

impl Default for DispatchOptions {
    fn default() -> Self {
        Self {
            ready_timeout: Duration::from_secs(20),
            ready_poll_interval: Duration::from_millis(250),
            ready_settle_delay: Duration::from_millis(300),
            ready_fallback_delay: Duration::from_millis(1500),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("dispatch target is required")]
    TargetRequired,
    #[error("dispatch sender is required")]
    SenderRequired,
    #[error("load config: {0}")]
    LoadConfig(String),
    #[error("load config tree: {0}")]
    LoadTree(String),
    #[error("build desired dispatch state: {0}")]
    BuildDesired(String),
    #[error("dispatch target {target:?} is not defined in {config}")]
    TargetNotDefined { target: String, config: String },
    #[error("orchestrator {target:?} does not support fresh restart because it is not a managed session")]
    FreshRestartUnsupported { target: String },
    #[error("orchestrator {target:?} is not running and is not a managed session")]
    UnmanagedTargetNotRunning { target: String },
    #[error(transparent)]
    Workspace(#[from] crate::WorkspaceError),
    #[error(transparent)]
    Orchestrator(#[from] crate::OrchestratorError),
    #[error("wake {target:?}: {source}")]
    Wake {
        target: String,
        #[source]
        source: ax_tmux::TmuxError,
    },
    #[error(
        "max_concurrent_agents cap reached: {count} live ax sessions vs cap {cap}; stop an agent or raise max_concurrent_agents"
    )]
    ConcurrentCapReached { count: u32, cap: u32 },
    #[error("query tmux sessions for capacity check: {0}")]
    CapacityQuery(#[source] ax_tmux::TmuxError),
}

/// Returns `Ok(())` when a new spawn would stay under the cap. A cap
/// of 0 disables the check so power-users can opt out. The query
/// itself can fail (tmux dead); those propagate as
/// `DispatchError::CapacityQuery` so callers don't silently spawn.
pub fn enforce_capacity_cap<B: TmuxBackend>(tmux: &B, cap: u32) -> Result<(), DispatchError> {
    if cap == 0 {
        return Ok(());
    }
    let count = tmux
        .list_sessions()
        .map_err(DispatchError::CapacityQuery)?
        .len() as u32;
    if count >= cap {
        return Err(DispatchError::ConcurrentCapReached { count, cap });
    }
    Ok(())
}

pub trait DispatchBackend: TmuxBackend {
    fn wake_workspace(&self, workspace: &str, prompt: &str) -> Result<(), ax_tmux::TmuxError>;
}

impl DispatchBackend for RealTmux {
    fn wake_workspace(&self, workspace: &str, prompt: &str) -> Result<(), ax_tmux::TmuxError> {
        ax_tmux::wake_workspace(workspace, prompt)
    }
}

pub fn dispatch_runnable_work<B: DispatchBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target: &str,
    sender: &str,
    fresh: bool,
) -> Result<(), DispatchError> {
    dispatch_runnable_work_with_options(
        tmux,
        socket_path,
        config_path,
        ax_bin,
        target,
        sender,
        fresh,
        DispatchOptions::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_runnable_work_with_options<B: DispatchBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target: &str,
    sender: &str,
    fresh: bool,
    options: DispatchOptions,
) -> Result<(), DispatchError> {
    let target = target.trim();
    if target.is_empty() {
        return Err(DispatchError::TargetRequired);
    }
    let sender = sender.trim();
    if sender.is_empty() {
        return Err(DispatchError::SenderRequired);
    }

    let needs_startup_sync = fresh || !tmux.session_exists(target);
    ensure_dispatch_target(tmux, socket_path, config_path, ax_bin, target, fresh)?;
    if needs_startup_sync {
        wait_for_dispatch_target_ready(tmux, target, options);
    }
    tmux.wake_workspace(target, &wake_prompt(sender, fresh))
        .map_err(|source| DispatchError::Wake {
            target: target.to_owned(),
            source,
        })?;
    Ok(())
}

pub fn ensure_dispatch_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target: &str,
    fresh: bool,
) -> Result<(), DispatchError> {
    let target = target.trim();
    if target.is_empty() {
        return Err(DispatchError::TargetRequired);
    }
    if !fresh && tmux.session_exists(target) {
        return Ok(());
    }

    let desired = load_dispatch_desired_state(socket_path, config_path)?;
    // Only creates (not restarts of live sessions) push count up, so
    // check the cap when the target session isn't already live.
    if !tmux.session_exists(target) {
        enforce_capacity_cap(tmux, desired.max_concurrent_agents)?;
    }
    if let Some(entry) = desired.workspaces.get(target) {
        return ensure_workspace_dispatch_target(
            tmux,
            socket_path,
            config_path,
            ax_bin,
            entry,
            fresh,
        );
    }
    if let Some(entry) = desired.orchestrators.get(target) {
        return ensure_orchestrator_dispatch_target(
            tmux,
            socket_path,
            config_path,
            ax_bin,
            entry,
            fresh,
        );
    }
    if !fresh && tmux.session_exists(target) {
        return Ok(());
    }
    Err(DispatchError::TargetNotDefined {
        target: target.to_owned(),
        config: clean_path(&config_path.display().to_string()),
    })
}

pub fn load_dispatch_desired_state(
    socket_path: &Path,
    config_path: &Path,
) -> Result<DesiredState, DispatchError> {
    let cfg = Config::load(config_path).map_err(|e| DispatchError::LoadConfig(e.to_string()))?;
    let tree =
        Config::load_tree(config_path).map_err(|e| DispatchError::LoadTree(e.to_string()))?;
    let include_root = !tree.disable_root_orchestrator;
    build_desired_state_with_tree(
        &cfg,
        &tree,
        socket_path.to_path_buf(),
        config_path.to_path_buf(),
        include_root,
    )
    .map_err(|e| DispatchError::BuildDesired(e.to_string()))
}

fn ensure_workspace_dispatch_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    entry: &DesiredWorkspace,
    fresh: bool,
) -> Result<(), DispatchError> {
    let manager = Manager::with_tmux(
        socket_path.to_path_buf(),
        Some(config_path.to_path_buf()),
        ax_bin.to_path_buf(),
        tmux.clone(),
    );
    if fresh {
        manager.restart(&entry.name, &entry.workspace)?;
        return Ok(());
    }
    if tmux.session_exists(&entry.name) {
        return Ok(());
    }
    manager.create(&entry.name, &entry.workspace)?;
    Ok(())
}

fn ensure_orchestrator_dispatch_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    entry: &DesiredOrchestrator,
    fresh: bool,
) -> Result<(), DispatchError> {
    if !entry.managed_session {
        if fresh {
            return Err(DispatchError::FreshRestartUnsupported {
                target: entry.name.clone(),
            });
        }
        if tmux.session_exists(&entry.name) {
            return Ok(());
        }
        return Err(DispatchError::UnmanagedTargetNotRunning {
            target: entry.name.clone(),
        });
    }

    if fresh {
        cleanup_orchestrator_state(tmux, &entry.name, &entry.artifact_dir)?;
    }
    if tmux.session_exists(&entry.name) {
        return Ok(());
    }
    ensure_orchestrator(
        tmux,
        &entry.node,
        &entry.parent_name,
        socket_path,
        Some(config_path),
        ax_bin,
        true,
    )?;
    Ok(())
}

fn wait_for_dispatch_target_ready<B: TmuxBackend>(
    tmux: &B,
    target: &str,
    options: DispatchOptions,
) {
    let deadline = Instant::now() + options.ready_timeout;
    while Instant::now() < deadline {
        if tmux.is_idle(target) {
            if !options.ready_settle_delay.is_zero() {
                thread::sleep(options.ready_settle_delay);
            }
            return;
        }
        if !options.ready_poll_interval.is_zero() {
            thread::sleep(options.ready_poll_interval);
        }
    }
    if !options.ready_fallback_delay.is_zero() {
        thread::sleep(options.ready_fallback_delay);
    }
}

fn wake_prompt(sender: &str, fresh: bool) -> String {
    let base = format!(
        "대기 중인 메시지가 있습니다. `read_messages`로 확인하고 요청된 작업을 수행해 주세요. `read_messages`가 비어 있어도 현재 워크스페이스에 할당된 daemon task를 `list_tasks(assignee=<self>, status=\"pending\")` 및 `list_tasks(assignee=<self>, status=\"in_progress\")`로 확인하고, runnable task는 `get_task`로 구조화된 문맥을 확인한 뒤 처리하세요. 회신이 필요하고 `{sender}`가 지원되는 `send_message` 대상임이 확실하면 `send_message(to=\"{sender}\")`로 결과를 보내고, 그렇지 않으면 현재 최종 응답 또는 지원되는 상위 reply path로 결과를 보고하세요."
    );
    if !fresh {
        return base;
    }
    base + " 이번 dispatch는 fresh-context 시작이 요청된 task입니다. 메시지에 `Task ID:`가 있으면 먼저 `get_task`로 해당 task를 확인하고, 이전 대화 문맥을 이어받았다고 가정하지 말고 현재 메시지와 task 정보만으로 다시 시작해 주세요."
}

fn clean_path(path: &str) -> String {
    if path.trim().is_empty() {
        return String::new();
    }
    normalize_path(Path::new(path)).display().to_string()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let is_absolute = path.is_absolute();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !is_absolute {
                    out.push("..");
                }
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }

    if out.as_os_str().is_empty() {
        if is_absolute {
            PathBuf::from("/")
        } else {
            PathBuf::from(".")
        }
    } else {
        out
    }
}
