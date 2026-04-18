//! Sidebar entry builder — turns the active sessions + optional
//! config tree into a flat list of renderable rows. Mirrors the
//! `buildSidebarEntries` / `buildSidebarFromTree` / `appendProjectEntries`
//! chain in `cmd/watch_sidebar.go`.
//!
//! Split out from rendering so tests can assert the tree expansion
//! without touching ratatui. Fancier features (agent-state spinner,
//! token summary, attention badge) depend on state we don't populate
//! yet and land in later slices.

use std::collections::BTreeMap;

use ax_config::ProjectNode;
use ax_tmux::SessionInfo;

/// One row in the sidebar. Group entries carry only a label; session
/// entries carry a workspace + session index so the selection
/// pointer can move across them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarEntry {
    pub label: String,
    pub workspace: String,
    pub session_index: Option<usize>,
    pub level: usize,
    pub group: bool,
    pub reconcile: String,
}

impl SidebarEntry {
    fn group(label: impl Into<String>, level: usize) -> Self {
        Self {
            label: label.into(),
            workspace: String::new(),
            session_index: None,
            level,
            group: true,
            reconcile: String::new(),
        }
    }

    fn leaf(
        label: impl Into<String>,
        workspace: impl Into<String>,
        session_index: Option<usize>,
        level: usize,
        reconcile: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            workspace: workspace.into(),
            session_index,
            level,
            group: false,
            reconcile: reconcile.into(),
        }
    }
}

/// Build sidebar entries from active tmux sessions + an optional
/// project tree + an optional reconfigure-desired set. When no tree
/// is available the fallback splits workspace names on `.` / `_`
/// into nested groups.
pub(crate) fn build_entries(
    sessions: &[SessionInfo],
    tree: Option<&ProjectNode>,
    reconfigure_enabled: bool,
    desired: &BTreeMap<String, bool>,
) -> Vec<SidebarEntry> {
    if let Some(node) = tree {
        return build_from_tree(node, sessions, reconfigure_enabled, desired);
    }
    build_fallback(sessions)
}

fn build_from_tree(
    root: &ProjectNode,
    sessions: &[SessionInfo],
    reconfigure_enabled: bool,
    desired: &BTreeMap<String, bool>,
) -> Vec<SidebarEntry> {
    let session_by_workspace: BTreeMap<&str, usize> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| (s.workspace.as_str(), i))
        .collect();

    let mut known: BTreeMap<String, bool> = BTreeMap::new();
    collect_known(root, &mut known);

    let mut entries = Vec::new();
    append_project(root, 0, &session_by_workspace, &mut entries, desired);

    let unregistered: Vec<usize> = sessions
        .iter()
        .enumerate()
        .filter_map(|(i, s)| (!known.contains_key(&s.workspace)).then_some(i))
        .collect();
    if !unregistered.is_empty() {
        entries.push(SidebarEntry::group(
            runtime_only_group_label(reconfigure_enabled),
            0,
        ));
        for idx in unregistered {
            let name = &sessions[idx].workspace;
            entries.push(SidebarEntry::leaf(
                name.clone(),
                name.clone(),
                Some(idx),
                1,
                reconfigure_sidebar_state(name, desired, true, false),
            ));
        }
    }
    entries
}

fn collect_known(node: &ProjectNode, known: &mut BTreeMap<String, bool>) {
    if root_orchestrator_visible(node) {
        known.insert(config_orchestrator_name(&node.prefix), true);
    }
    for ws in &node.workspaces {
        known.insert(ws.merged_name.clone(), true);
    }
    for child in &node.children {
        collect_known(child, known);
    }
}

fn append_project(
    node: &ProjectNode,
    level: usize,
    session_by_workspace: &BTreeMap<&str, usize>,
    entries: &mut Vec<SidebarEntry>,
    desired: &BTreeMap<String, bool>,
) {
    entries.push(SidebarEntry::group(
        format!("▾ {}", node.display_name()),
        level,
    ));

    if root_orchestrator_visible(node) {
        let orch_name = config_orchestrator_name(&node.prefix);
        let is_root_orch = node.prefix.is_empty() && orch_name == "orchestrator";
        let session_idx = session_by_workspace.get(orch_name.as_str()).copied();
        let reconcile_offline = if is_root_orch {
            String::new()
        } else {
            reconfigure_sidebar_state(&orch_name, desired, false, false)
        };
        let reconcile = if session_idx.is_some() {
            reconfigure_sidebar_state(&orch_name, desired, true, false)
        } else {
            reconcile_offline
        };
        entries.push(SidebarEntry::leaf(
            "◆ orchestrator",
            orch_name,
            session_idx,
            level + 1,
            reconcile,
        ));
    }

    for ws in &node.workspaces {
        let session_idx = session_by_workspace.get(ws.merged_name.as_str()).copied();
        let reconcile =
            reconfigure_sidebar_state(&ws.merged_name, desired, session_idx.is_some(), false);
        entries.push(SidebarEntry::leaf(
            ws.name.clone(),
            ws.merged_name.clone(),
            session_idx,
            level + 1,
            reconcile,
        ));
    }

    for child in &node.children {
        append_project(child, level + 1, session_by_workspace, entries, desired);
    }
}

fn build_fallback(sessions: &[SessionInfo]) -> Vec<SidebarEntry> {
    // No config available — split workspace names by "." or "_" and
    // render as a nested tree so sibling workspaces share a header.
    let mut root = Node::default();
    for (i, session) in sessions.iter().enumerate() {
        let parts = split_workspace_path(&session.workspace);
        let mut cursor = &mut root;
        let last = parts.len().saturating_sub(1);
        for (depth, part) in parts.iter().enumerate() {
            let child = cursor.children.entry(part.clone()).or_insert_with(|| Node {
                name: part.clone(),
                ..Default::default()
            });
            if depth == last {
                child.session_index = Some(i);
            }
            cursor = child;
        }
    }

    let mut entries = Vec::new();
    walk_fallback(&root, 0, &mut entries);
    entries
}

fn walk_fallback(node: &Node, level: usize, entries: &mut Vec<SidebarEntry>) {
    for child in node.children.values() {
        if !child.children.is_empty() {
            entries.push(SidebarEntry::group(format!("▾ {}", child.name), level));
        }
        if let Some(idx) = child.session_index {
            entries.push(SidebarEntry::leaf(
                child.name.clone(),
                child.name.clone(),
                Some(idx),
                level,
                String::new(),
            ));
        }
        if !child.children.is_empty() {
            walk_fallback(child, level + 1, entries);
        }
    }
}

#[derive(Default)]
struct Node {
    name: String,
    session_index: Option<usize>,
    children: BTreeMap<String, Node>,
}

fn split_workspace_path(workspace: &str) -> Vec<String> {
    if workspace.contains('.') {
        workspace.split('.').map(ToOwned::to_owned).collect()
    } else if workspace.matches('_').count() >= 2 {
        workspace.split('_').map(ToOwned::to_owned).collect()
    } else {
        vec![workspace.to_owned()]
    }
}

fn root_orchestrator_visible(node: &ProjectNode) -> bool {
    !(node.prefix.is_empty() && node.disable_root_orchestrator)
}

fn config_orchestrator_name(prefix: &str) -> String {
    if prefix.is_empty() {
        "orchestrator".to_owned()
    } else {
        format!("{prefix}.orchestrator")
    }
}

fn runtime_only_group_label(enabled: bool) -> &'static str {
    if enabled {
        "▾ runtime-only (not in config tree)"
    } else {
        "▾ unregistered (not in config tree)"
    }
}

/// Reconcile state label used by the sidebar. Mirrors Go's
/// `reconfigureSidebarState`: desired-only / runtime-only / (empty).
fn reconfigure_sidebar_state(
    name: &str,
    desired: &BTreeMap<String, bool>,
    has_session: bool,
    has_agent: bool,
) -> String {
    if desired.is_empty() {
        return String::new();
    }
    match (desired.contains_key(name), has_session || has_agent) {
        (true, false) => "desired".to_owned(),
        (false, true) if !name.starts_with('_') => "runtime-only".to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ax_config::WorkspaceRef;

    fn session(workspace: &str) -> SessionInfo {
        SessionInfo {
            name: format!("ax-{workspace}"),
            workspace: workspace.to_owned(),
            attached: false,
            windows: 1,
        }
    }

    fn project(prefix: &str, workspaces: Vec<&str>, children: Vec<ProjectNode>) -> ProjectNode {
        ProjectNode {
            name: if prefix.is_empty() {
                "root".into()
            } else {
                prefix.into()
            },
            alias: String::new(),
            prefix: prefix.into(),
            dir: std::path::PathBuf::new(),
            orchestrator_runtime: String::new(),
            disable_root_orchestrator: false,
            workspaces: workspaces
                .into_iter()
                .map(|name| WorkspaceRef {
                    name: name.into(),
                    merged_name: if prefix.is_empty() {
                        name.into()
                    } else {
                        format!("{prefix}.{name}")
                    },
                    runtime: String::new(),
                    description: String::new(),
                    instructions: String::new(),
                })
                .collect(),
            children,
        }
    }

    #[test]
    fn fallback_splits_dotted_workspace_names_into_nested_groups() {
        let entries = build_fallback(&[session("team.backend"), session("team.frontend")]);
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, vec!["▾ team", "backend", "frontend"]);
        assert!(!entries[0].group || entries[0].session_index.is_none());
        assert_eq!(entries[1].session_index, Some(0));
        assert_eq!(entries[2].session_index, Some(1));
    }

    #[test]
    fn tree_renders_orchestrator_then_workspaces_then_children() {
        let child = project("team", vec!["worker"], vec![]);
        let root = project("", vec!["alpha"], vec![child]);
        let sessions = vec![session("orchestrator"), session("alpha")];
        let entries = build_from_tree(&root, &sessions, false, &BTreeMap::new());
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "▾ root",
                "◆ orchestrator",
                "alpha",
                "▾ team",
                "◆ orchestrator",
                "worker",
            ]
        );
        // Only "orchestrator" (root) is live; alpha/team child aren't running.
        assert_eq!(entries[1].session_index, Some(0));
        assert_eq!(entries[2].session_index, Some(1));
        assert_eq!(entries[4].session_index, None);
        assert_eq!(entries[5].session_index, None);
    }

    #[test]
    fn tree_appends_unregistered_sessions_under_runtime_only_group() {
        let root = project("", vec!["alpha"], vec![]);
        let sessions = vec![session("alpha"), session("ghost")];
        let entries = build_from_tree(&root, &sessions, false, &BTreeMap::new());
        let last = entries.last().unwrap();
        assert_eq!(last.workspace, "ghost");
        assert_eq!(last.session_index, Some(1));
        assert!(entries
            .iter()
            .any(|e| e.label.starts_with("▾ unregistered")));
    }

    #[test]
    fn reconfigure_state_flags_desired_only_and_runtime_only() {
        let mut desired = BTreeMap::new();
        desired.insert("alpha".to_owned(), true);
        desired.insert("beta".to_owned(), true);
        assert_eq!(
            reconfigure_sidebar_state("alpha", &desired, false, false),
            "desired"
        );
        assert_eq!(
            reconfigure_sidebar_state("alpha", &desired, true, false),
            ""
        );
        assert_eq!(
            reconfigure_sidebar_state("ghost", &desired, true, false),
            "runtime-only"
        );
        assert_eq!(reconfigure_sidebar_state("_cli", &desired, true, false), "");
    }
}
