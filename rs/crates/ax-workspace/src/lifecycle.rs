//! Targeted lifecycle control for managed sessions.
//!
//! Mirrors `internal/workspace/lifecycle.go`.

use std::path::Path;
use std::path::{Component, PathBuf};

use ax_proto::types::{LifecycleAction, LifecycleTarget, LifecycleTargetKind};

use crate::{
    cleanup_orchestrator_state, ensure_orchestrator, load_dispatch_desired_state,
    DesiredOrchestrator, DesiredWorkspace, Manager, TmuxBackend,
};

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("target name is required")]
    TargetRequired,
    #[error(transparent)]
    Dispatch(#[from] crate::DispatchError),
    #[error("target {target:?} is ambiguous in {config} because it matches both a workspace and an orchestrator")]
    AmbiguousTarget { target: String, config: String },
    #[error("target {target:?} is not defined in {config}")]
    TargetNotDefined { target: String, config: String },
    #[error("{kind} {name:?} is already running")]
    AlreadyRunning { kind: &'static str, name: String },
    #[error("{kind} {name:?} is not running")]
    NotRunning { kind: &'static str, name: String },
    #[error(
        "{kind} {name:?} does not support targeted {action} because it is not a managed session"
    )]
    UnsupportedManagedSession {
        kind: &'static str,
        name: String,
        action: &'static str,
    },
    #[error("destroy tmux session: {0}")]
    DestroySession(#[source] ax_tmux::TmuxError),
    #[error(transparent)]
    Workspace(#[from] crate::WorkspaceError),
    #[error(transparent)]
    Orchestrator(#[from] crate::OrchestratorError),
}

#[derive(Debug, Clone)]
struct ResolvedLifecycleTarget {
    target: LifecycleTarget,
    workspace: Option<DesiredWorkspace>,
    orchestrator: Option<DesiredOrchestrator>,
}

pub fn start_named_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target: &str,
) -> Result<LifecycleTarget, LifecycleError> {
    control_named_target(
        tmux,
        socket_path,
        config_path,
        ax_bin,
        target,
        LifecycleAction::Start,
    )
}

pub fn stop_named_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target: &str,
) -> Result<LifecycleTarget, LifecycleError> {
    control_named_target(
        tmux,
        socket_path,
        config_path,
        ax_bin,
        target,
        LifecycleAction::Stop,
    )
}

pub fn restart_named_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target: &str,
) -> Result<LifecycleTarget, LifecycleError> {
    control_named_target(
        tmux,
        socket_path,
        config_path,
        ax_bin,
        target,
        LifecycleAction::Restart,
    )
}

fn control_named_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target_name: &str,
    action: LifecycleAction,
) -> Result<LifecycleTarget, LifecycleError> {
    let target = resolve_lifecycle_target(socket_path, config_path, target_name)?;
    match &target.target.kind {
        LifecycleTargetKind::Workspace => {
            control_workspace_target(tmux, socket_path, config_path, ax_bin, &target, action)?
        }
        LifecycleTargetKind::Orchestrator => {
            control_orchestrator_target(tmux, socket_path, config_path, ax_bin, &target, action)?
        }
    }
    Ok(target.target)
}

fn resolve_lifecycle_target(
    socket_path: &Path,
    config_path: &Path,
    target_name: &str,
) -> Result<ResolvedLifecycleTarget, LifecycleError> {
    let target_name = target_name.trim();
    if target_name.is_empty() {
        return Err(LifecycleError::TargetRequired);
    }

    let desired = load_dispatch_desired_state(socket_path, config_path)?;
    let workspace = desired.workspaces.get(target_name).cloned();
    let orchestrator = desired.orchestrators.get(target_name).cloned();

    match (workspace, orchestrator) {
        (Some(_), Some(_)) => Err(LifecycleError::AmbiguousTarget {
            target: target_name.to_owned(),
            config: clean_path(&config_path.display().to_string()),
        }),
        (Some(workspace), None) => Ok(ResolvedLifecycleTarget {
            target: LifecycleTarget {
                name: workspace.name.clone(),
                kind: LifecycleTargetKind::Workspace,
                managed_session: true,
            },
            workspace: Some(workspace),
            orchestrator: None,
        }),
        (None, Some(orchestrator)) => Ok(ResolvedLifecycleTarget {
            target: LifecycleTarget {
                name: orchestrator.name.clone(),
                kind: LifecycleTargetKind::Orchestrator,
                managed_session: orchestrator.managed_session,
            },
            workspace: None,
            orchestrator: Some(orchestrator),
        }),
        (None, None) => Err(LifecycleError::TargetNotDefined {
            target: target_name.to_owned(),
            config: clean_path(&config_path.display().to_string()),
        }),
    }
}

fn control_workspace_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target: &ResolvedLifecycleTarget,
    action: LifecycleAction,
) -> Result<(), LifecycleError> {
    let workspace = target.workspace.as_ref().expect("workspace target");
    let manager = Manager::with_tmux(
        socket_path.to_path_buf(),
        Some(config_path.to_path_buf()),
        ax_bin.to_path_buf(),
        tmux.clone(),
    );

    match action {
        LifecycleAction::Start => {
            if tmux.session_exists(&target.target.name) {
                return Err(LifecycleError::AlreadyRunning {
                    kind: "workspace",
                    name: target.target.name.clone(),
                });
            }
            manager.create(&target.target.name, &workspace.workspace)?;
            Ok(())
        }
        LifecycleAction::Stop => stop_session_target(tmux, &target.target),
        LifecycleAction::Restart => {
            manager.restart(&target.target.name, &workspace.workspace)?;
            Ok(())
        }
    }
}

fn control_orchestrator_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target: &ResolvedLifecycleTarget,
    action: LifecycleAction,
) -> Result<(), LifecycleError> {
    let orchestrator = target.orchestrator.as_ref().expect("orchestrator target");
    if !target.target.managed_session {
        return Err(LifecycleError::UnsupportedManagedSession {
            kind: "orchestrator",
            name: target.target.name.clone(),
            action: lifecycle_action_name(&action),
        });
    }

    match action {
        LifecycleAction::Start => {
            if tmux.session_exists(&target.target.name) {
                return Err(LifecycleError::AlreadyRunning {
                    kind: "orchestrator",
                    name: target.target.name.clone(),
                });
            }
            ensure_orchestrator(
                tmux,
                &orchestrator.node,
                &orchestrator.parent_name,
                socket_path,
                Some(config_path),
                ax_bin,
                true,
            )?;
            Ok(())
        }
        LifecycleAction::Stop => stop_session_target(tmux, &target.target),
        LifecycleAction::Restart => {
            cleanup_orchestrator_state(tmux, &target.target.name, &orchestrator.artifact_dir)?;
            ensure_orchestrator(
                tmux,
                &orchestrator.node,
                &orchestrator.parent_name,
                socket_path,
                Some(config_path),
                ax_bin,
                true,
            )?;
            Ok(())
        }
    }
}

fn stop_session_target<B: TmuxBackend>(
    tmux: &B,
    target: &LifecycleTarget,
) -> Result<(), LifecycleError> {
    if !tmux.session_exists(&target.name) {
        return Err(LifecycleError::NotRunning {
            kind: lifecycle_kind_name(&target.kind),
            name: target.name.clone(),
        });
    }
    tmux.destroy_session(&target.name)
        .map_err(LifecycleError::DestroySession)?;
    Ok(())
}

fn lifecycle_kind_name(kind: &LifecycleTargetKind) -> &'static str {
    match kind {
        LifecycleTargetKind::Workspace => "workspace",
        LifecycleTargetKind::Orchestrator => "orchestrator",
    }
}

fn lifecycle_action_name(action: &LifecycleAction) -> &'static str {
    match action {
        LifecycleAction::Start => "start",
        LifecycleAction::Stop => "stop",
        LifecycleAction::Restart => "restart",
    }
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
