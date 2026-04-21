//! Rendering + helpers for `ax status`. Covers task summaries,
//! workspace/agent status lines, status previews, and the
//! config-tree pretty printer. Consumes the sync `DaemonClient` in
//! [`crate::daemon_client`] and `ax_tmux::list_sessions`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ax_config::{Config, ProjectNode};
use ax_proto::types::{
    AgentStatus, Task, TaskPriority, TaskStatus, WorkspaceGitStatus, WorkspaceInfo,
};
use ax_tmux::SessionInfo;

use crate::daemon_client::DaemonClient;

#[derive(Debug, Default, Clone)]
pub(crate) struct TaskSummary {
    pub total: i64,
    pub pending: i64,
    pub in_progress: i64,
    pub completed: i64,
    pub failed: i64,
    pub cancelled: i64,
    pub stale: i64,
    pub diverged: i64,
    pub queued_messages: i64,
    pub urgent_or_high: i64,
    pub recoverable: i64,
    pub top_attention_ids: Vec<String>,
}

pub(crate) fn summarize_tasks(tasks: &[Task]) -> TaskSummary {
    let mut summary = TaskSummary {
        total: tasks.len() as i64,
        ..TaskSummary::default()
    };
    let mut top_attention: Vec<&Task> = Vec::new();
    for task in tasks {
        match task.status {
            TaskStatus::Pending => summary.pending += 1,
            TaskStatus::InProgress => summary.in_progress += 1,
            TaskStatus::Completed => summary.completed += 1,
            TaskStatus::Failed => summary.failed += 1,
            TaskStatus::Cancelled => summary.cancelled += 1,
            TaskStatus::Blocked => {}
        }
        if matches!(
            task.priority,
            Some(TaskPriority::Urgent | TaskPriority::High)
        ) {
            summary.urgent_or_high += 1;
        }
        if task_is_stale(task) {
            summary.stale += 1;
        }
        if let Some(info) = &task.stale_info {
            summary.queued_messages += info.pending_messages;
            if info.state_divergence {
                summary.diverged += 1;
            }
            if info.is_stale || info.state_divergence {
                summary.recoverable += 1;
                top_attention.push(task);
            }
        }
    }

    top_attention.sort_by(|a, b| {
        let pa = task_priority_order(a.priority.as_ref());
        let pb = task_priority_order(b.priority.as_ref());
        pa.cmp(&pb)
            .then_with(|| task_is_stale(b).cmp(&task_is_stale(a)))
            .then_with(|| a.updated_at.cmp(&b.updated_at))
    });
    for task in top_attention.iter().take(3) {
        summary.top_attention_ids.push(short_task_id(&task.id));
    }
    summary
}

pub(crate) fn format_task_summary(summary: &TaskSummary) -> String {
    let mut parts = vec![
        format!("total={}", summary.total),
        format!("pending={}", summary.pending),
        format!("in_progress={}", summary.in_progress),
        format!("stale={}", summary.stale),
        format!("diverged={}", summary.diverged),
        format!("queued_msgs={}", summary.queued_messages),
    ];
    if summary.cancelled > 0 {
        parts.push(format!("cancelled={}", summary.cancelled));
    }
    if summary.urgent_or_high > 0 {
        parts.push(format!("high_pri={}", summary.urgent_or_high));
    }
    if !summary.top_attention_ids.is_empty() {
        parts.push(format!("attention={}", summary.top_attention_ids.join(",")));
    }
    parts.join("  ")
}

pub(crate) fn task_attention_hint(summary: &TaskSummary) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if summary.diverged > 0 {
        parts.push(format!("{} diverged", summary.diverged));
    }
    if summary.stale > 0 {
        parts.push(format!("{} stale", summary.stale));
    }
    if summary.queued_messages > 0 {
        parts.push(format!("{} queued message(s)", summary.queued_messages));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!(
            "Attention: {}. Inspect with ax tasks --stale, ax tasks show <id>, or ax workspace list.",
            parts.join(", ")
        ))
    }
}

fn task_is_stale(task: &Task) -> bool {
    if let Some(info) = &task.stale_info {
        if info.is_stale {
            return true;
        }
    }
    if !matches!(task.status, TaskStatus::Pending | TaskStatus::InProgress) {
        return false;
    }
    if task.stale_after_seconds <= 0 {
        return false;
    }
    let elapsed = (chrono::Utc::now() - task.updated_at).num_seconds();
    elapsed >= task.stale_after_seconds
}

fn task_priority_order(priority: Option<&TaskPriority>) -> i32 {
    match priority {
        Some(TaskPriority::Urgent) => 0,
        Some(TaskPriority::High) => 1,
        None | Some(TaskPriority::Normal) => 2,
        Some(TaskPriority::Low) => 3,
    }
}

fn short_task_id(id: &str) -> String {
    if id.chars().count() > 8 {
        id.chars().take(8).collect()
    } else {
        id.to_owned()
    }
}

#[must_use]
pub(crate) fn workspace_info_map(workspaces: &[WorkspaceInfo]) -> BTreeMap<String, WorkspaceInfo> {
    workspaces
        .iter()
        .map(|ws| (ws.name.clone(), ws.clone()))
        .collect()
}

pub(crate) fn workspace_agent_status(
    workspaces: &BTreeMap<String, WorkspaceInfo>,
    name: &str,
) -> &'static str {
    match workspaces.get(name).map(|ws| &ws.status) {
        Some(AgentStatus::Online) => "online",
        Some(AgentStatus::Disconnected) => "disconnected",
        Some(AgentStatus::Offline) | None => "offline",
    }
}

pub(crate) fn workspace_status_preview(
    workspaces: &BTreeMap<String, WorkspaceInfo>,
    name: &str,
    limit: usize,
) -> String {
    let Some(info) = workspaces.get(name) else {
        return String::new();
    };
    let trimmed = info.status_text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    truncate_str(trimmed, limit)
}

pub(crate) fn workspace_git_status_preview(
    workspaces: &BTreeMap<String, WorkspaceInfo>,
    name: &str,
    limit: usize,
) -> String {
    let Some(git) = workspaces
        .get(name)
        .and_then(|info| info.git_status.as_ref())
    else {
        return String::new();
    };
    truncate_str(&format_git_status(git), limit)
}

fn format_git_status(git: &WorkspaceGitStatus) -> String {
    let state = normalized_git_state(git);
    match state.as_str() {
        "non_git" => return git_message("git non-git", git),
        "inaccessible" => return git_message("git inaccessible", git),
        "error" => return git_message("git error", git),
        _ => {}
    }

    let mut out = format!(
        "git {state} M{} A{} D{} ?{}",
        git.modified, git.added, git.deleted, git.untracked
    );
    if git.files_changed > 0 || git.insertions > 0 || git.deletions > 0 {
        let _ = write!(
            out,
            " · {} files +{} -{}",
            git.files_changed, git.insertions, git.deletions
        );
    }
    out
}

fn normalized_git_state(git: &WorkspaceGitStatus) -> String {
    let state = git.state.trim();
    if !state.is_empty() {
        return state.to_owned();
    }
    if git.modified + git.added + git.deleted + git.untracked > 0 {
        "dirty".to_owned()
    } else {
        "clean".to_owned()
    }
}

fn git_message(prefix: &str, git: &WorkspaceGitStatus) -> String {
    if git.message.trim().is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}: {}", git.message.trim())
    }
}

fn truncate_str(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_owned();
    }
    let mut out: String = chars[..n].iter().collect();
    out.push('…');
    out
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

fn collect_known_workspaces(node: &ProjectNode, known: &mut BTreeSet<String>) {
    if root_orchestrator_visible(node) {
        known.insert(config_orchestrator_name(&node.prefix));
    }
    for ws in &node.workspaces {
        known.insert(ws.merged_name.clone());
    }
    for child in &node.children {
        collect_known_workspaces(child, known);
    }
}

fn reconfigure_tmux_state(agent_status: &str, enabled: bool) -> &'static str {
    if enabled && agent_status == "offline" {
        return "desired";
    }
    if agent_status != "offline" {
        return "no-session";
    }
    "offline"
}

fn runtime_only_group_label(enabled: bool) -> &'static str {
    if enabled {
        "▾ runtime-only (not in config tree)"
    } else {
        "▾ unregistered (not in config tree)"
    }
}

pub(crate) fn render_status(
    socket_path: &Path,
    config_path_override: Option<&Path>,
    daemon_running: bool,
) -> Result<String, StatusError> {
    let mut out = String::new();
    writeln!(
        out,
        "Daemon: {}",
        if daemon_running { "running" } else { "stopped" }
    )
    .expect("write");

    let mut workspace_infos: BTreeMap<String, WorkspaceInfo> = BTreeMap::new();
    let mut task_summary = TaskSummary::default();

    if daemon_running {
        match DaemonClient::connect(socket_path, "_cli") {
            Ok(mut client) => {
                match client.list_tasks("", "", None) {
                    Ok(tasks) => {
                        task_summary = summarize_tasks(&tasks);
                        let _ = writeln!(out, "Tasks: {}", format_task_summary(&task_summary));
                    }
                    Err(e) => {
                        let _ = writeln!(out, "Tasks: unavailable ({e})");
                    }
                }
                match client.list_workspaces() {
                    Ok(workspaces) => {
                        workspace_infos = workspace_info_map(&workspaces);
                        let _ = writeln!(out, "Agents: {} online", workspace_infos.len());
                    }
                    Err(e) => {
                        let _ = writeln!(out, "Agents: unavailable ({e})");
                    }
                }
                if let Some(text) = task_attention_hint(&task_summary) {
                    let _ = writeln!(out, "{text}");
                }
            }
            Err(e) => {
                let _ = writeln!(out, "Tasks: unavailable ({e})");
                let _ = writeln!(out, "Agents: unavailable ({e})");
            }
        }
    }

    let sessions = ax_tmux::list_sessions().map_err(StatusError::Tmux)?;
    let session_by_workspace: BTreeMap<String, SessionInfo> = sessions
        .iter()
        .map(|s| (s.workspace.clone(), s.clone()))
        .collect();

    let _ = write!(out, "\nWorkspaces: {} active", sessions.len());
    if daemon_running && !workspace_infos.is_empty() {
        let _ = write!(
            out,
            " ({} agent connection(s) online)",
            workspace_infos.len()
        );
    }
    out.push_str("\n\n");

    let cfg_path = resolve_config_path(config_path_override);
    if let Some(path) = cfg_path {
        if let Ok(tree) = Config::load_tree(&path) {
            let reconfigure_enabled = Config::load(&path)
                .map(|cfg| cfg.experimental_mcp_team_reconfigure)
                .unwrap_or(false);
            let mut known: BTreeSet<String> = BTreeSet::new();
            collect_known_workspaces(&tree, &mut known);
            if reconfigure_enabled {
                let _ = writeln!(
                    out,
                    "Reconfigure: desired-only entries are configured but not running; runtime-only entries are outside {}\n",
                    path.display()
                );
            }
            print_project_tree(
                &mut out,
                &tree,
                0,
                &session_by_workspace,
                &workspace_infos,
                reconfigure_enabled,
            );

            let unregistered: Vec<&SessionInfo> = sessions
                .iter()
                .filter(|s| !known.contains(&s.workspace))
                .collect();
            if !unregistered.is_empty() {
                let _ = writeln!(out, "\n{}", runtime_only_group_label(reconfigure_enabled));
                for session in unregistered {
                    let status = if session.attached {
                        "attached"
                    } else {
                        "detached"
                    };
                    let agent_status = workspace_agent_status(&workspace_infos, &session.workspace);
                    let git_status =
                        workspace_git_status_preview(&workspace_infos, &session.workspace, 72);
                    let status_text =
                        workspace_status_preview(&workspace_infos, &session.workspace, 72);
                    let _ = write!(
                        out,
                        "  ● {:<26} {:<10} {:<8} {}",
                        session.workspace, status, agent_status, session.name
                    );
                    if !git_status.is_empty() {
                        let _ = write!(out, " | {git_status}");
                    }
                    if !status_text.is_empty() {
                        let _ = write!(out, " | {status_text}");
                    }
                    out.push('\n');
                }
                if reconfigure_enabled {
                    out.push_str(
                        "\nReview runtime-only leftovers before treating the reconfiguration as reconciled.\n",
                    );
                } else {
                    out.push_str("\nRun 'ax init' in the project directory to register these.\n");
                }
            }
            return Ok(out);
        }
    }

    if !sessions.is_empty() {
        let _ = writeln!(
            out,
            "{:<24} {:<10} {:<8} {:<18} INFO",
            "NAME", "TMUX", "AGENT", "SESSION"
        );
        let _ = writeln!(
            out,
            "{:<24} {:<10} {:<8} {:<18} -----------",
            "----", "----", "-----", "-------"
        );
        for session in &sessions {
            let status = if session.attached {
                "attached"
            } else {
                "detached"
            };
            let git_status = workspace_git_status_preview(&workspace_infos, &session.workspace, 48);
            let status_text = workspace_status_preview(&workspace_infos, &session.workspace, 64);
            let info = match (git_status.is_empty(), status_text.is_empty()) {
                (true, true) => String::new(),
                (false, true) => git_status,
                (true, false) => status_text,
                (false, false) => format!("{git_status} | {status_text}"),
            };
            let _ = writeln!(
                out,
                "{:<24} {:<10} {:<8} {:<18} {}",
                session.workspace,
                status,
                workspace_agent_status(&workspace_infos, &session.workspace),
                session.name,
                info,
            );
        }
    }
    Ok(out)
}

fn print_project_tree(
    out: &mut String,
    node: &ProjectNode,
    level: usize,
    sessions: &BTreeMap<String, SessionInfo>,
    workspaces: &BTreeMap<String, WorkspaceInfo>,
    reconfigure_enabled: bool,
) {
    let indent = "  ".repeat(level);
    let _ = writeln!(out, "{indent}▾ {}", node.display_name());

    if root_orchestrator_visible(node) {
        let name = config_orchestrator_name(&node.prefix);
        let allow_desired = !(node.prefix.is_empty() && name == "orchestrator");
        print_leaf(
            out,
            level + 1,
            "◆ orchestrator",
            &name,
            sessions,
            workspaces,
            reconfigure_enabled && allow_desired,
        );
    }
    for ws in &node.workspaces {
        print_leaf(
            out,
            level + 1,
            &ws.name,
            &ws.merged_name,
            sessions,
            workspaces,
            reconfigure_enabled,
        );
    }
    for child in &node.children {
        print_project_tree(
            out,
            child,
            level + 1,
            sessions,
            workspaces,
            reconfigure_enabled,
        );
    }
}

fn print_leaf(
    out: &mut String,
    level: usize,
    label: &str,
    merged_name: &str,
    sessions: &BTreeMap<String, SessionInfo>,
    workspaces: &BTreeMap<String, WorkspaceInfo>,
    reconfigure_enabled: bool,
) {
    let indent = "  ".repeat(level);
    let agent_status = workspace_agent_status(workspaces, merged_name);
    let git_status = workspace_git_status_preview(workspaces, merged_name, 72);
    let status_text = workspace_status_preview(workspaces, merged_name, 72);
    if let Some(session) = sessions.get(merged_name) {
        let status = if session.attached {
            "attached"
        } else {
            "detached"
        };
        let _ = write!(
            out,
            "{indent}● {:<26} {:<10} {:<8} {}",
            label, status, agent_status, session.name
        );
        if !git_status.is_empty() {
            let _ = write!(out, " | {git_status}");
        }
        if !status_text.is_empty() {
            let _ = write!(out, " | {status_text}");
        }
        out.push('\n');
    } else {
        let tmux_status = reconfigure_tmux_state(agent_status, reconfigure_enabled);
        let _ = write!(
            out,
            "{indent}○ {label:<26} {tmux_status:<10} {agent_status:<8}"
        );
        if !git_status.is_empty() {
            let _ = write!(out, " {git_status}");
        }
        if !status_text.is_empty() {
            let _ = write!(out, " {status_text}");
        }
        out.push('\n');
    }
}

fn resolve_config_path(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return Some(path.to_path_buf());
    }
    ax_config::find_config_file(std::env::current_dir().ok()?)
}

#[derive(Debug)]
pub(crate) enum StatusError {
    Tmux(ax_tmux::TmuxError),
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tmux(e) => write!(f, "list tmux sessions: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn task(
        id: &str,
        status: TaskStatus,
        priority: Option<TaskPriority>,
        stale_info_divergence: bool,
        pending_messages: i64,
    ) -> Task {
        let now = Utc::now();
        let stale_info = if stale_info_divergence || pending_messages > 0 {
            Some(ax_proto::types::TaskStaleInfo {
                is_stale: false,
                reason: String::new(),
                recommended_action: String::new(),
                last_progress_at: now,
                age_seconds: 0,
                pending_messages,
                last_message_at: None,
                wake_pending: false,
                wake_attempts: 0,
                next_wake_retry_at: None,
                claim_state: String::new(),
                claim_state_note: String::new(),
                runnable: false,
                runnable_reason: String::new(),
                recovery_eligible: false,
                state_divergence: stale_info_divergence,
                state_divergence_note: String::new(),
            })
        } else {
            None
        };
        Task {
            id: id.to_owned(),
            title: id.to_owned(),
            description: String::new(),
            assignee: "alpha".to_owned(),
            created_by: "orch".to_owned(),
            parent_task_id: String::new(),
            child_task_ids: Vec::new(),
            version: 1,
            status,
            start_mode: ax_proto::types::TaskStartMode::Default,
            workflow_mode: None,
            priority,
            stale_after_seconds: 0,
            dispatch_message: String::new(),
            dispatch_config_path: String::new(),
            dispatch_count: 0,
            attempt_count: 0,
            last_dispatch_at: None,
            last_attempt_at: None,
            next_retry_at: None,
            claimed_at: None,
            claimed_by: String::new(),
            claim_source: String::new(),
            result: String::new(),
            logs: Vec::new(),
            rollup: None,
            sequence: None,
            stale_info,
            removed_at: None,
            removed_by: String::new(),
            remove_reason: String::new(),
            created_at: now,
            updated_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
        }
    }

    #[test]
    fn summarize_counts_by_status_and_priority() {
        let tasks = vec![
            task(
                "a",
                TaskStatus::Pending,
                Some(TaskPriority::Urgent),
                false,
                0,
            ),
            task(
                "b",
                TaskStatus::InProgress,
                Some(TaskPriority::High),
                false,
                2,
            ),
            task("c", TaskStatus::Completed, None, false, 0),
            task("d", TaskStatus::Cancelled, None, false, 0),
            task("e", TaskStatus::InProgress, None, true, 0),
        ];
        let summary = summarize_tasks(&tasks);
        assert_eq!(summary.total, 5);
        assert_eq!(summary.pending, 1);
        assert_eq!(summary.in_progress, 2);
        assert_eq!(summary.completed, 1);
        assert_eq!(summary.cancelled, 1);
        assert_eq!(summary.urgent_or_high, 2);
        assert_eq!(summary.queued_messages, 2);
        assert_eq!(summary.diverged, 1);
        assert_eq!(summary.recoverable, 1);
        assert_eq!(summary.top_attention_ids, vec!["e".to_owned()]);
    }

    #[test]
    fn format_task_summary_matches_go_shape() {
        let summary = TaskSummary {
            total: 3,
            pending: 1,
            in_progress: 1,
            cancelled: 1,
            stale: 0,
            diverged: 0,
            queued_messages: 0,
            urgent_or_high: 1,
            top_attention_ids: vec!["abc12345".into()],
            ..TaskSummary::default()
        };
        assert_eq!(
            format_task_summary(&summary),
            "total=3  pending=1  in_progress=1  stale=0  diverged=0  queued_msgs=0  cancelled=1  high_pri=1  attention=abc12345"
        );
    }

    #[test]
    fn attention_hint_emits_only_when_non_trivial() {
        let clean = TaskSummary::default();
        assert!(task_attention_hint(&clean).is_none());
        let with = TaskSummary {
            stale: 2,
            queued_messages: 3,
            ..TaskSummary::default()
        };
        let text = task_attention_hint(&with).expect("hint");
        assert!(text.contains("2 stale"));
        assert!(text.contains("3 queued message(s)"));
    }

    #[test]
    fn truncate_str_adds_ellipsis_when_over_budget() {
        assert_eq!(truncate_str("hello world", 5), "hello…");
        assert_eq!(truncate_str("abc", 10), "abc");
        assert_eq!(truncate_str("abc", 0), "");
    }

    #[test]
    fn workspace_status_preview_trims_and_truncates() {
        let mut map = BTreeMap::new();
        map.insert(
            "alpha".to_owned(),
            WorkspaceInfo {
                name: "alpha".into(),
                dir: "/tmp".into(),
                description: String::new(),
                status: AgentStatus::Online,
                status_text: "  long running job that is definitely more than limit".into(),
                git_status: None,
                connected_at: None,
                last_activity_at: None,
                active_task_count: 0,
                current_task_id: None,
            },
        );
        let preview = workspace_status_preview(&map, "alpha", 10);
        assert_eq!(preview, "long runni…");
        assert_eq!(workspace_status_preview(&map, "missing", 10), "");
    }

    #[test]
    fn workspace_git_status_preview_includes_counts_and_diffstat() {
        let mut map = BTreeMap::new();
        map.insert(
            "alpha".to_owned(),
            WorkspaceInfo {
                name: "alpha".into(),
                dir: "/tmp".into(),
                description: String::new(),
                status: AgentStatus::Online,
                status_text: String::new(),
                git_status: Some(WorkspaceGitStatus {
                    state: "dirty".into(),
                    modified: 2,
                    added: 1,
                    deleted: 0,
                    untracked: 3,
                    files_changed: 4,
                    insertions: 10,
                    deletions: 2,
                    message: String::new(),
                }),
                connected_at: None,
                last_activity_at: None,
                active_task_count: 0,
                current_task_id: None,
            },
        );

        let preview = workspace_git_status_preview(&map, "alpha", 80);
        assert_eq!(preview, "git dirty M2 A1 D0 ?3 · 4 files +10 -2");
    }

    #[test]
    fn workspace_git_status_preview_handles_non_git_message() {
        let mut map = BTreeMap::new();
        map.insert(
            "alpha".to_owned(),
            WorkspaceInfo {
                name: "alpha".into(),
                dir: "/tmp".into(),
                description: String::new(),
                status: AgentStatus::Online,
                status_text: String::new(),
                git_status: Some(WorkspaceGitStatus {
                    state: "non_git".into(),
                    message: "no .git directory".into(),
                    ..WorkspaceGitStatus::default()
                }),
                connected_at: None,
                last_activity_at: None,
                active_task_count: 0,
                current_task_id: None,
            },
        );

        let preview = workspace_git_status_preview(&map, "alpha", 80);
        assert_eq!(preview, "git non-git: no .git directory");
    }
}
