//! `ax refresh` — refreshes generated ax artifacts (workspace MCP
//! config + instructions, orchestrator prompts) and optionally
//! reconciles tmux sessions.
//!
//! Two shapes:
//! - Experimental team-reconfigure mode: delegates to the Rust
//!   Reconciler directly and prints a report.
//! - Normal mode: ensures per-workspace artifacts, optionally
//!   restart/start-missing via [`Manager`], then walks the
//!   orchestrator tree to refresh artifacts or recreate sessions.

use std::fmt::Write as _;
use std::path::Path;

use ax_config::{Config, ProjectNode};
use ax_workspace::{
    build_desired_state_with_tree, cleanup_orchestrator_state, ensure_artifacts,
    ensure_orchestrator_tree, orchestrator_name, root_orchestrator_dir, Manager, OrchestratorError,
    RealTmux, ReconcileOptions, ReconcileReport, Reconciler, WorkspaceError as AxWorkspaceError,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RefreshOptions {
    pub restart: bool,
    pub start_missing: bool,
}

pub(crate) fn run(
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    daemon_running: bool,
    opts: RefreshOptions,
) -> Result<String, RefreshError> {
    let cfg = Config::load(config_path).map_err(RefreshError::LoadConfig)?;
    let tree = Config::load_tree(config_path).map_err(RefreshError::LoadTree)?;

    let mut out = String::new();
    let _ = writeln!(out, "Config: {}", config_path.display());
    let _ = writeln!(
        out,
        "Daemon: {}",
        if daemon_running { "running" } else { "stopped" }
    );

    if cfg.experimental_mcp_team_reconfigure {
        let skip_root =
            reconcile_root_orchestrator_state(&cfg).map_err(RefreshError::Orchestrator)?;
        let desired = build_desired_state_with_tree(
            &cfg,
            &tree,
            socket_path.to_path_buf(),
            config_path.to_path_buf(),
            !skip_root,
        )
        .map_err(|e| RefreshError::Reconcile(e.to_string()))?;
        let reconciler = Reconciler::new(socket_path, config_path, ax_bin);
        let report = reconciler
            .reconcile_desired_state(
                &desired,
                ReconcileOptions {
                    daemon_running,
                    allow_disruptive_changes: true,
                },
            )
            .map_err(|e| RefreshError::Reconcile(e.to_string()))?;
        write_reconcile_report(&mut out, &report);
        return Ok(out);
    }

    let manager: Manager = Manager::new(
        socket_path.to_path_buf(),
        Some(config_path.to_path_buf()),
        ax_bin.to_path_buf(),
    );

    let mut names: Vec<&String> = cfg.workspaces.keys().collect();
    names.sort();

    out.push_str("\nWorkspaces:\n");
    for name in names {
        let ws = cfg
            .workspaces
            .get(name)
            .expect("workspace from sorted iter exists");
        ensure_artifacts(name, ws, socket_path, Some(config_path), ax_bin)
            .map_err(|e| RefreshError::Workspace(format!("refresh workspace {name:?}: {e}")))?;

        let exists = ax_tmux::session_exists(name);
        if opts.restart && exists {
            manager
                .destroy(name, &ws.dir)
                .map_err(|e| RefreshError::Workspace(format!("restart workspace {name:?}: {e}")))?;
            manager
                .create(name, ws)
                .map_err(|e| RefreshError::Workspace(format!("restart workspace {name:?}: {e}")))?;
            let _ = writeln!(out, "  {name}: artifacts refreshed, session restarted");
        } else if opts.start_missing && daemon_running && !exists {
            manager
                .create(name, ws)
                .map_err(|e| RefreshError::Workspace(format!("start workspace {name:?}: {e}")))?;
            let _ = writeln!(out, "  {name}: artifacts refreshed, session started");
        } else if exists {
            let _ = writeln!(out, "  {name}: artifacts refreshed, session unchanged");
        } else {
            let _ = writeln!(out, "  {name}: artifacts refreshed, session offline");
        }
    }

    out.push_str("\nOrchestrators:\n");
    let skip_root = reconcile_root_orchestrator_state(&cfg).map_err(RefreshError::Orchestrator)?;

    if opts.restart {
        destroy_orchestrator_sessions(&mut out, &tree);
        if daemon_running {
            ensure_orchestrator_tree(
                &RealTmux,
                &tree,
                socket_path,
                Some(config_path),
                ax_bin,
                true,
                skip_root,
            )
            .map_err(RefreshError::Orchestrator)?;
        } else {
            ensure_orchestrator_tree(
                &RealTmux,
                &tree,
                socket_path,
                Some(config_path),
                ax_bin,
                false,
                skip_root,
            )
            .map_err(RefreshError::Orchestrator)?;
        }
        out.push_str("  tree: artifacts refreshed, running orchestrators restarted\n");
    } else if opts.start_missing && daemon_running {
        ensure_orchestrator_tree(
            &RealTmux,
            &tree,
            socket_path,
            Some(config_path),
            ax_bin,
            true,
            skip_root,
        )
        .map_err(RefreshError::Orchestrator)?;
        out.push_str("  tree: artifacts refreshed, missing orchestrators started\n");
    } else {
        ensure_orchestrator_tree(
            &RealTmux,
            &tree,
            socket_path,
            Some(config_path),
            ax_bin,
            false,
            skip_root,
        )
        .map_err(RefreshError::Orchestrator)?;
        out.push_str("  tree: artifacts refreshed, sessions unchanged\n");
    }

    if !opts.restart {
        out.push_str(
            "\nNote: running sessions keep their current agent process. Use --restart to apply runtime changes immediately.\n",
        );
    }
    Ok(out)
}

fn write_reconcile_report(out: &mut String, report: &ReconcileReport) {
    out.push_str("\nExperimental Runtime Reconcile:\n");
    if report.actions.is_empty() {
        out.push_str("  no runtime/workspace/orchestrator changes\n");
    } else {
        for action in &report.actions {
            let mut line = format!("  {} {}: {}", action.kind, action.name, action.operation);
            if !action.details.is_empty() {
                line.push_str(" (");
                line.push_str(&action.details);
                line.push(')');
            }
            let _ = writeln!(out, "{line}");
        }
    }
    if report.root_manual_restart_required {
        out.push_str(
            "\nNote: root foreground orchestrator requires manual relaunch to pick up artifact changes.\n",
        );
    }
}

fn destroy_orchestrator_sessions(out: &mut String, node: &ProjectNode) {
    for child in &node.children {
        destroy_orchestrator_sessions(out, child);
    }
    let name = orchestrator_name(&node.prefix);
    if ax_tmux::session_exists(&name) {
        let _ = ax_tmux::destroy_session(&name);
        let _ = writeln!(out, "  {name}: stopped");
    }
}

/// If the config disables the root orchestrator, tear down its
/// generated artifacts and tell callers to skip the root node during
/// ensure loops.
fn reconcile_root_orchestrator_state(cfg: &Config) -> Result<bool, OrchestratorError> {
    if !cfg.disable_root_orchestrator {
        return Ok(false);
    }
    let root_dir = root_orchestrator_dir()?;
    cleanup_orchestrator_state(&RealTmux, &orchestrator_name(""), &root_dir)?;
    Ok(true)
}

#[derive(Debug)]
pub(crate) enum RefreshError {
    LoadConfig(ax_config::TreeError),
    LoadTree(ax_config::TreeError),
    Workspace(String),
    Reconcile(String),
    Orchestrator(OrchestratorError),
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadConfig(e) => write!(f, "load ax config: {e}"),
            Self::LoadTree(e) => write!(f, "load config tree: {e}"),
            Self::Workspace(msg) => f.write_str(msg),
            Self::Reconcile(msg) => write!(f, "reconcile: {msg}"),
            Self::Orchestrator(e) => write!(f, "{e}"),
        }
    }
}

impl From<AxWorkspaceError> for RefreshError {
    fn from(e: AxWorkspaceError) -> Self {
        Self::Workspace(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ax_workspace::{ReconcileAction, ReconcileReport};

    #[test]
    fn reconcile_report_without_actions_prints_empty_summary() {
        let mut out = String::new();
        write_reconcile_report(&mut out, &ReconcileReport::default());
        assert!(out.contains("no runtime/workspace/orchestrator changes"));
    }

    #[test]
    fn reconcile_report_with_actions_lists_them_and_notes_manual_restart() {
        let mut out = String::new();
        let report = ReconcileReport {
            actions: vec![ReconcileAction {
                kind: "workspace".into(),
                name: "alpha".into(),
                operation: "create".into(),
                details: "was missing".into(),
            }],
            root_manual_restart_required: true,
            root_manual_restart_reasons: vec!["codex prompt changed".into()],
        };
        write_reconcile_report(&mut out, &report);
        assert!(out.contains("workspace alpha: create (was missing)"));
        assert!(out.contains("root foreground orchestrator requires manual relaunch"));
    }
}
