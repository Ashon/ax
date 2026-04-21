//! `ax workspace` — `create`, `destroy`, `list`, `attach`,
//! `interrupt` subcommands. The `list` view renders the workspace
//! rows plus experimental reconfigure-state annotations so stale
//! terminals still parse.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ax_config::{Config, ProjectNode};
use ax_proto::types::WorkspaceInfo;
use ax_tmux::SessionInfo;

use crate::daemon_client::DaemonClient;
use crate::status::{workspace_agent_status, workspace_info_map, workspace_status_preview};

/// Flags shared by the `list` subcommand. Kept plain so tests can
/// instantiate it without going through argv parsing.
#[derive(Debug, Default, Clone)]
pub(crate) struct ListOptions {
    pub include_internal: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceListRow {
    pub name: String,
    pub reconcile: String,
    pub tmux: String,
    pub agent: String,
    pub status_text: String,
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct WorkspaceListView {
    pub reconfigure_enabled: bool,
    pub rows: Vec<WorkspaceListRow>,
    pub hidden_internal: Vec<String>,
}

pub(crate) fn build_workspace_list_rows(
    sessions: &[SessionInfo],
    workspaces: &BTreeMap<String, WorkspaceInfo>,
    descriptions: &BTreeMap<String, String>,
    desired: &BTreeSet<String>,
    reconfigure_enabled: bool,
    include_internal: bool,
) -> WorkspaceListView {
    let mut session_by_workspace: BTreeMap<String, SessionInfo> = BTreeMap::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    for session in sessions {
        session_by_workspace.insert(session.workspace.clone(), session.clone());
        names.insert(session.workspace.clone());
    }
    for name in workspaces.keys() {
        names.insert(name.clone());
    }
    for name in desired {
        names.insert(name.clone());
    }

    let mut view = WorkspaceListView {
        reconfigure_enabled,
        rows: Vec::with_capacity(names.len()),
        hidden_internal: Vec::new(),
    };
    for name in &names {
        let has_agent = workspaces.contains_key(name);
        let has_session = session_by_workspace.contains_key(name);
        let mut row = WorkspaceListRow {
            name: name.clone(),
            reconcile: reconfigure_row_state(name, desired, has_session, has_agent),
            tmux: String::new(),
            agent: workspace_agent_status(workspaces, name).to_owned(),
            status_text: workspace_status_preview(workspaces, name, 40),
            description: descriptions.get(name).cloned().unwrap_or_default(),
        };

        if let Some(session) = session_by_workspace.get(name) {
            if session.attached {
                "attached"
            } else {
                "detached"
            }
            .clone_into(&mut row.tmux);
            if !has_agent {
                "no-agent".clone_into(&mut row.agent);
            }
        } else {
            "no-session".clone_into(&mut row.tmux);
        }

        if is_internal_daemon_only_row(&row) {
            if !include_internal {
                view.hidden_internal.push(row.name);
                continue;
            }
            if row.description.is_empty() {
                "internal daemon identity".clone_into(&mut row.description);
            } else {
                row.description.push_str(" (internal)");
            }
        }
        view.rows.push(row);
    }
    view
}

fn reconfigure_row_state(
    name: &str,
    desired: &BTreeSet<String>,
    has_session: bool,
    has_agent: bool,
) -> String {
    if desired.is_empty() {
        return String::new();
    }
    if desired.contains(name) && !has_session && !has_agent {
        return "desired-only".to_owned();
    }
    if !desired.contains(name) && (has_session || has_agent) && !name.starts_with('_') {
        return "runtime-only".to_owned();
    }
    "configured".to_owned()
}

fn is_internal_daemon_only_row(row: &WorkspaceListRow) -> bool {
    row.tmux == "no-session" && row.name.starts_with('_')
}

pub(crate) fn format_hidden_internal_note(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let (label, pronoun) = if names.len() == 1 {
        ("workspace", "it")
    } else {
        ("workspaces", "them")
    };
    format!(
        "Hidden {} internal daemon-only {label}: {}. Use --internal to show {pronoun}.",
        names.len(),
        names.join(", ")
    )
}

pub(crate) fn collect_desired_workspace_names(node: &ProjectNode, out: &mut BTreeSet<String>) {
    if !(node.prefix.is_empty() && node.disable_root_orchestrator) {
        let name = if node.prefix.is_empty() {
            "orchestrator".to_owned()
        } else {
            format!("{}.orchestrator", node.prefix)
        };
        out.insert(name);
    }
    for ws in &node.workspaces {
        out.insert(ws.merged_name.clone());
    }
    for child in &node.children {
        collect_desired_workspace_names(child, out);
    }
}

pub(crate) fn render_list(
    socket_path: &Path,
    configured_config: Option<&Path>,
    daemon_running: bool,
    opts: &ListOptions,
) -> Result<String, WorkspaceCliError> {
    let sessions = ax_tmux::list_sessions().map_err(WorkspaceCliError::Tmux)?;
    let mut descriptions: BTreeMap<String, String> = BTreeMap::new();
    let mut desired: BTreeSet<String> = BTreeSet::new();
    let mut reconfigure_enabled = false;

    let cfg_path = resolve_config_path(configured_config);
    if let Some(path) = &cfg_path {
        if let Ok(cfg) = Config::load(path) {
            for (name, ws) in &cfg.workspaces {
                descriptions.insert(name.clone(), ws.description.clone());
                desired.insert(name.clone());
            }
            if cfg.experimental_mcp_team_reconfigure {
                reconfigure_enabled = true;
                if let Ok(tree) = Config::load_tree(path) {
                    desired.clear();
                    collect_desired_workspace_names(&tree, &mut desired);
                }
            }
        }
    }

    let mut workspace_infos: BTreeMap<String, WorkspaceInfo> = BTreeMap::new();
    if daemon_running {
        if let Ok(mut client) = DaemonClient::connect(socket_path, "_cli") {
            if let Ok(workspaces) = client.list_workspaces() {
                workspace_infos = workspace_info_map(&workspaces);
            }
        }
    }

    let view = build_workspace_list_rows(
        &sessions,
        &workspace_infos,
        &descriptions,
        &desired,
        reconfigure_enabled,
        opts.include_internal,
    );

    let mut out = String::new();
    if view.rows.is_empty() {
        out.push_str("No workspaces found.\n");
        let note = format_hidden_internal_note(&view.hidden_internal);
        if !note.is_empty() {
            let _ = writeln!(out, "{note}");
        }
        return Ok(out);
    }

    if view.reconfigure_enabled {
        let _ = writeln!(
            out,
            "{:<22} {:<14} {:<10} {:<8} {:<40} DESCRIPTION",
            "WORKSPACE", "RECONCILE", "TMUX", "AGENT", "STATUS TEXT"
        );
        let _ = writeln!(
            out,
            "{:<22} {:<14} {:<10} {:<8} {:<40} -----------",
            "---------", "---------", "----", "-----", "-----------"
        );
        for row in &view.rows {
            let _ = writeln!(
                out,
                "{:<22} {:<14} {:<10} {:<8} {:<40} {}",
                row.name, row.reconcile, row.tmux, row.agent, row.status_text, row.description
            );
        }
    } else {
        let _ = writeln!(
            out,
            "{:<22} {:<10} {:<8} {:<40} DESCRIPTION",
            "WORKSPACE", "TMUX", "AGENT", "STATUS TEXT"
        );
        let _ = writeln!(
            out,
            "{:<22} {:<10} {:<8} {:<40} -----------",
            "---------", "----", "-----", "-----------"
        );
        for row in &view.rows {
            let _ = writeln!(
                out,
                "{:<22} {:<10} {:<8} {:<40} {}",
                row.name, row.tmux, row.agent, row.status_text, row.description
            );
        }
    }

    if view.reconfigure_enabled {
        out.push_str("\nReconcile state is relative to the active config tree.\n");
    }
    let note = format_hidden_internal_note(&view.hidden_internal);
    if !note.is_empty() {
        let _ = write!(out, "\n{note}\n");
    }
    Ok(out)
}

pub(crate) fn create_workspace(
    socket_path: &Path,
    config_path: Option<&Path>,
    ax_bin: &Path,
    name: &str,
    dir: Option<PathBuf>,
) -> Result<String, WorkspaceCliError> {
    let dir = match dir {
        Some(path) => path,
        None => std::env::current_dir().map_err(WorkspaceCliError::Cwd)?,
    };
    let ws = ax_config::Workspace {
        dir: dir.display().to_string(),
        ..Default::default()
    };
    let manager = ax_workspace::Manager::new(
        socket_path.to_path_buf(),
        config_path.map(Path::to_path_buf),
        ax_bin.to_path_buf(),
    );
    manager
        .create(name, &ws)
        .map_err(WorkspaceCliError::Workspace)?;
    Ok(format!(
        "Workspace {name:?} created (session: {}, dir: {})\nAttach with: ax workspace attach {name}\n",
        ax_tmux::session_name(name),
        dir.display()
    ))
}

pub(crate) fn destroy_workspace(
    socket_path: &Path,
    config_path: Option<&Path>,
    ax_bin: &Path,
    name: &str,
) -> Result<String, WorkspaceCliError> {
    let manager = ax_workspace::Manager::new(
        socket_path.to_path_buf(),
        config_path.map(Path::to_path_buf),
        ax_bin.to_path_buf(),
    );
    manager
        .destroy(name, "")
        .map_err(WorkspaceCliError::Workspace)?;
    Ok(format!("Workspace {name:?} destroyed\n"))
}

pub(crate) fn resolve_attach_target(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    if ax_tmux::session_exists(trimmed) {
        return Some(trimmed.to_owned());
    }
    let orch = ax_workspace::orchestrator_name(trimmed);
    if orch != trimmed && ax_tmux::session_exists(&orch) {
        return Some(orch);
    }
    None
}

pub(crate) fn attach(name: &str) -> Result<(), WorkspaceCliError> {
    let Some(target) = resolve_attach_target(name) else {
        let orch = ax_workspace::orchestrator_name(name.trim());
        return Err(WorkspaceCliError::NotFound {
            name: name.to_owned(),
            expected_session: ax_tmux::session_name(name),
            orchestrator: orch.clone(),
            orchestrator_session: ax_tmux::session_name(&orch),
        });
    };
    ax_tmux::attach_session(&target).map_err(WorkspaceCliError::Tmux)
}

pub(crate) fn interrupt(name: &str) -> Result<String, WorkspaceCliError> {
    if !ax_tmux::session_exists(name) {
        return Err(WorkspaceCliError::InterruptMissing {
            name: name.to_owned(),
            expected_session: ax_tmux::session_name(name),
        });
    }
    ax_tmux::interrupt_workspace(name).map_err(WorkspaceCliError::Tmux)?;
    Ok(format!("Workspace {name:?} interrupted\n"))
}

fn resolve_config_path(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return Some(path.to_path_buf());
    }
    ax_config::find_config_file(std::env::current_dir().ok()?)
}

#[derive(Debug)]
pub(crate) enum WorkspaceCliError {
    Cwd(std::io::Error),
    Tmux(ax_tmux::TmuxError),
    Workspace(ax_workspace::WorkspaceError),
    NotFound {
        name: String,
        expected_session: String,
        orchestrator: String,
        orchestrator_session: String,
    },
    InterruptMissing {
        name: String,
        expected_session: String,
    },
}

impl std::fmt::Display for WorkspaceCliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cwd(e) => write!(f, "resolve current dir: {e}"),
            Self::Tmux(e) => write!(f, "{e}"),
            Self::Workspace(e) => write!(f, "{e}"),
            Self::NotFound {
                name,
                expected_session,
                orchestrator,
                orchestrator_session,
            } => write!(
                f,
                "workspace {name:?} not found (no tmux session {expected_session}; tried project orchestrator {orchestrator:?} -> {orchestrator_session})"
            ),
            Self::InterruptMissing {
                name,
                expected_session,
            } => write!(
                f,
                "workspace {name:?} not found (no tmux session {expected_session})"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ax_proto::types::AgentStatus;

    fn info(name: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            name: name.to_owned(),
            dir: "/tmp".into(),
            description: String::new(),
            status: AgentStatus::Online,
            status_text: String::new(),
            git_status: None,
            connected_at: None,
            last_activity_at: None,
            active_task_count: 0,
            current_task_id: None,
            connection_generation: 0,
            idle_timeout_seconds: 0,
        }
    }

    fn session(workspace: &str, attached: bool) -> SessionInfo {
        SessionInfo {
            name: format!("ax-{workspace}"),
            workspace: workspace.to_owned(),
            attached,
            windows: 1,
        }
    }

    #[test]
    fn build_rows_marks_runtime_only_and_desired_only_in_reconfigure_mode() {
        let sessions = vec![session("extra", false)];
        let mut workspaces = BTreeMap::new();
        workspaces.insert("alpha".to_owned(), info("alpha"));
        let mut descriptions = BTreeMap::new();
        descriptions.insert("alpha".into(), "alpha workspace".into());
        let mut desired = BTreeSet::new();
        desired.insert("alpha".to_owned());
        desired.insert("beta".to_owned());

        let view =
            build_workspace_list_rows(&sessions, &workspaces, &descriptions, &desired, true, false);
        assert!(view.reconfigure_enabled);
        let rows: Vec<_> = view
            .rows
            .iter()
            .map(|r| (r.name.as_str(), r.reconcile.as_str(), r.tmux.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("alpha", "configured", "no-session"),
                ("beta", "desired-only", "no-session"),
                ("extra", "runtime-only", "detached"),
            ]
        );
    }

    #[test]
    fn internal_rows_hidden_by_default_and_surfaced_with_flag() {
        let sessions: Vec<SessionInfo> = Vec::new();
        let mut workspaces = BTreeMap::new();
        workspaces.insert("_cli".to_owned(), info("_cli"));
        let descriptions = BTreeMap::new();
        let desired = BTreeSet::new();

        let hidden = build_workspace_list_rows(
            &sessions,
            &workspaces,
            &descriptions,
            &desired,
            false,
            false,
        );
        assert!(hidden.rows.is_empty());
        assert_eq!(hidden.hidden_internal, vec!["_cli".to_owned()]);

        let shown =
            build_workspace_list_rows(&sessions, &workspaces, &descriptions, &desired, false, true);
        assert_eq!(shown.rows.len(), 1);
        assert_eq!(shown.rows[0].description, "internal daemon identity");
    }

    #[test]
    fn hidden_internal_note_switches_to_plural_labels() {
        assert_eq!(format_hidden_internal_note(&[]), "");
        let one = format_hidden_internal_note(&["_cli".to_owned()]);
        assert!(one.contains("1 internal"));
        assert!(one.contains("workspace"));
        assert!(one.contains("show it"));
        let two = format_hidden_internal_note(&["_cli".to_owned(), "_dispatcher".to_owned()]);
        assert!(two.contains("2 internal"));
        assert!(two.contains("workspaces"));
        assert!(two.contains("show them"));
    }
}
