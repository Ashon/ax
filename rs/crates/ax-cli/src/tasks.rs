//! `ax-rs tasks` — port of cmd/tasks.go. Implements the list /
//! show / cancel / remove / recover / intervene / retry / activity
//! subcommands against the sync [`DaemonClient`]. Shares the task
//! summary + workspace status helpers with [`crate::status`].

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ax_daemon::{expand_socket_path, HistoryEntry};
use ax_proto::types::{Task, TaskLog, TaskPriority, TaskStartMode, TaskStatus};
use chrono::{DateTime, Utc};

use crate::daemon_client::{DaemonClient, DaemonClientError};
use crate::status::{format_task_summary, summarize_tasks};

/// Filter passed to `ax tasks --stale` and the watch TUI. Defaults
/// to `Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TaskFilterMode {
    Active,
    Stale,
    Done,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TasksCommand {
    List {
        assignee: String,
        created_by: String,
        status: Option<TaskStatus>,
        only_stale: bool,
    },
    Show {
        id: String,
        log_limit: usize,
    },
    Cancel {
        id: String,
        reason: String,
        expected_version: Option<i64>,
    },
    Remove {
        id: String,
        reason: String,
        expected_version: Option<i64>,
    },
    Recover {
        id: String,
    },
    Intervene {
        id: String,
        action: String,
        note: String,
        expected_version: Option<i64>,
    },
    Retry {
        id: String,
        note: String,
        expected_version: Option<i64>,
    },
    Activity {
        id: Option<String>,
        assignee: String,
        created_by: String,
        status: Option<TaskStatus>,
        only_stale: bool,
        limit: usize,
    },
}

/// Dispatch table: run a parsed `ax tasks` invocation against the
/// daemon and render its output. `socket_path` is only used for
/// locating the on-disk history file.
pub(crate) fn run(socket_path: &Path, command: TasksCommand) -> Result<String, TasksError> {
    let mut client = DaemonClient::connect(socket_path, "_cli").map_err(TasksError::Connect)?;
    match command {
        TasksCommand::List {
            assignee,
            created_by,
            status,
            only_stale,
        } => run_list(&mut client, &assignee, &created_by, status, only_stale),
        TasksCommand::Show { id, log_limit } => run_show(&mut client, socket_path, &id, log_limit),
        TasksCommand::Cancel {
            id,
            reason,
            expected_version,
        } => {
            let task = client
                .cancel_task(&id, &reason, expected_version)
                .map_err(TasksError::Request)?;
            Ok(format_mutation("Cancelled", &task))
        }
        TasksCommand::Remove {
            id,
            reason,
            expected_version,
        } => {
            let task = client
                .remove_task(&id, &reason, expected_version)
                .map_err(TasksError::Request)?;
            Ok(format_mutation("Removed", &task))
        }
        TasksCommand::Recover { id } => {
            let task = client.get_task(&id).map_err(TasksError::Request)?;
            Ok(task_recovery_preview_lines(&task).join("\n") + "\n")
        }
        TasksCommand::Intervene {
            id,
            action,
            note,
            expected_version,
        } => {
            let action = action.trim();
            if !matches!(action, "wake" | "interrupt" | "retry") {
                return Err(TasksError::InvalidAction(action.to_owned()));
            }
            let resp = client
                .intervene_task(&id, action, note.trim(), expected_version)
                .map_err(TasksError::Request)?;
            Ok(format_intervention(&resp))
        }
        TasksCommand::Retry {
            id,
            note,
            expected_version,
        } => {
            let resp = client
                .intervene_task(&id, "retry", note.trim(), expected_version)
                .map_err(TasksError::Request)?;
            Ok(format_intervention(&resp))
        }
        TasksCommand::Activity {
            id,
            assignee,
            created_by,
            status,
            only_stale,
            limit,
        } => run_activity(
            &mut client,
            socket_path,
            id.as_deref(),
            &assignee,
            &created_by,
            status,
            only_stale,
            limit,
        ),
    }
}

fn run_list(
    client: &mut DaemonClient,
    assignee: &str,
    created_by: &str,
    status: Option<TaskStatus>,
    only_stale: bool,
) -> Result<String, TasksError> {
    let mut tasks = client
        .list_tasks(assignee, created_by, status)
        .map_err(TasksError::Request)?;
    if only_stale {
        tasks.retain(task_is_stale);
    }
    sort_tasks_for_display(&mut tasks);
    if tasks.is_empty() {
        return Ok("No tasks found.\n".to_owned());
    }
    let summary = summarize_tasks(&tasks);
    let mut out = String::new();
    let _ = writeln!(out, "Summary: {}\n", format_task_summary(&summary));
    let _ = writeln!(
        out,
        "{:<8} {:<8} {:<18} {:<6} {:<16} {:<16} {:<24} NEXT SIGNAL",
        "ID", "PRI", "STATUS", "AGE", "ASSIGNEE", "CREATED BY", "TITLE"
    );
    for task in &tasks {
        let id = short_task_id(&task.id);
        let _ = writeln!(
            out,
            "{:<8} {:<8} {:<18} {:<6} {:<16} {:<16} {:<24} {}",
            id,
            truncate_str(&task_priority_label(task.priority.as_ref()), 8),
            truncate_str(&task_status_label(task), 18),
            format_task_age(task),
            truncate_str(&task.assignee, 16),
            truncate_str(&task.created_by, 16),
            truncate_str(&task.title, 24),
            truncate_str(&task_operator_hint(task).replace('\n', " "), 72)
        );
    }
    Ok(out)
}

fn run_show(
    client: &mut DaemonClient,
    socket_path: &Path,
    id: &str,
    log_limit: usize,
) -> Result<String, TasksError> {
    let task = client.get_task(id).map_err(TasksError::Request)?;
    let mut out = String::new();
    let _ = writeln!(out, "Task: {}", task.title);
    let _ = writeln!(out, "ID: {}", task.id);
    let _ = writeln!(out, "Status: {}", task_status_label(&task));
    let _ = writeln!(out, "Version: {}", task.version);
    let _ = writeln!(out, "Assignee: {}", task.assignee);
    let _ = writeln!(out, "Created By: {}", task.created_by);
    let _ = writeln!(
        out,
        "Priority: {}",
        task_priority_label(task.priority.as_ref())
    );
    let _ = writeln!(
        out,
        "Updated: {} ago ({})",
        format_task_age(&task),
        task.updated_at.format("%Y-%m-%d %H:%M:%S")
    );
    let _ = writeln!(
        out,
        "Created: {}",
        task.created_at.format("%Y-%m-%d %H:%M:%S")
    );
    let _ = writeln!(out, "Start Mode: {}", start_mode_label(&task.start_mode));
    if let Some(ts) = task.removed_at {
        let _ = writeln!(out, "Removed: {}", ts.format("%Y-%m-%d %H:%M:%S"));
        if !task.removed_by.is_empty() {
            let _ = writeln!(out, "Removed By: {}", task.removed_by);
        }
        if !task.remove_reason.is_empty() {
            let _ = writeln!(out, "Remove Reason: {}", task.remove_reason);
        }
    }
    if task.stale_after_seconds > 0 {
        let _ = writeln!(out, "Stale After: {}s", task.stale_after_seconds);
    }
    if !task.description.is_empty() {
        let _ = writeln!(out, "\nDescription:\n{}", task.description);
    }
    if !task.result.is_empty() {
        let _ = writeln!(out, "\nResult:\n{}", task.result);
    }
    if let Some(info) = &task.stale_info {
        out.push_str("\nStale Info:\n");
        let _ = writeln!(out, "- is_stale: {}", info.is_stale);
        let _ = writeln!(out, "- age: {}", format_age_seconds(info.age_seconds));
        if !info.reason.is_empty() {
            let _ = writeln!(out, "- reason: {}", info.reason);
        }
        if !info.recommended_action.is_empty() {
            let _ = writeln!(out, "- action: {}", info.recommended_action);
        }
        if info.pending_messages > 0 {
            let _ = writeln!(out, "- pending_messages: {}", info.pending_messages);
        }
        if info.state_divergence {
            let _ = writeln!(out, "- divergence: {}", info.state_divergence_note);
        }
        if let Some(ts) = info.last_message_at {
            let _ = writeln!(out, "- last_message: {}", ts.format("%Y-%m-%d %H:%M:%S"));
        }
        if info.wake_pending {
            let _ = writeln!(out, "- wake_pending: true");
            if info.wake_attempts > 0 {
                let _ = writeln!(out, "- wake_attempts: {}", info.wake_attempts);
            }
            if let Some(ts) = info.next_wake_retry_at {
                let _ = writeln!(out, "- next_wake_retry: {}", ts.format("%Y-%m-%d %H:%M:%S"));
            }
        }
    }

    let _ = writeln!(out, "\nOperator Hint:\n{}", task_operator_hint(&task));

    out.push_str("\nRecent Logs:\n");
    let logs = recent_task_logs(&task, log_limit);
    if logs.is_empty() {
        out.push_str("(none)\n");
    } else {
        for log in logs {
            let _ = writeln!(
                out,
                "- {} {}: {}",
                log.timestamp.format("%H:%M:%S"),
                log.workspace,
                log.message
            );
        }
    }

    out.push_str("\nRelated Messages:\n");
    let history = read_history_file(&history_file_path(socket_path), 200);
    let msgs = related_task_messages(&task, &history, 6);
    if msgs.is_empty() {
        out.push_str("(none)\n");
    } else {
        for msg in msgs {
            let content = msg.content.replace('\n', " ");
            let _ = writeln!(
                out,
                "- {} {} -> {}: {}",
                msg.timestamp.format("%H:%M:%S"),
                msg.from,
                msg.to,
                truncate_str(&content, 120)
            );
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn run_activity(
    client: &mut DaemonClient,
    socket_path: &Path,
    id: Option<&str>,
    assignee: &str,
    created_by: &str,
    status: Option<TaskStatus>,
    only_stale: bool,
    limit: usize,
) -> Result<String, TasksError> {
    let history = read_history_file(&history_file_path(socket_path), 500);
    if let Some(id) = id {
        let task = client.get_task(id).map_err(TasksError::Request)?;
        let mut out = String::new();
        let _ = writeln!(out, "Activity: {} ({})\n", task.title, task.id);
        let entries = build_task_activity(&task, &history, limit);
        if entries.is_empty() {
            out.push_str("(no activity)\n");
        } else {
            for entry in &entries {
                let _ = writeln!(
                    out,
                    "{} {:<12} {:<22} {}",
                    entry.timestamp.format("%Y-%m-%d %H:%M:%S"),
                    activity_kind_label(entry.kind),
                    truncate_str(&entry.actor, 22),
                    entry.summary
                );
                if !entry.detail.is_empty() {
                    let _ = writeln!(
                        out,
                        "  {}",
                        truncate_str(&entry.detail.replace('\n', " "), 140)
                    );
                }
            }
        }
        return Ok(out);
    }

    let mut tasks = client
        .list_tasks(assignee, created_by, status)
        .map_err(TasksError::Request)?;
    if only_stale {
        tasks.retain(task_is_stale);
    }
    if tasks.is_empty() {
        return Ok("No tasks found.\n".to_owned());
    }

    let mut entries: Vec<TaskActivityEntry> = Vec::new();
    for task in &tasks {
        for mut entry in build_task_activity(task, &history, 0) {
            entry.detail = format!("{} {}", short_task_id(&task.id), task.title);
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    if limit > 0 && entries.len() > limit {
        entries.truncate(limit);
    }

    let mut out = String::new();
    for entry in &entries {
        let _ = write!(
            out,
            "{} {:<12} {:<22} {}",
            entry.timestamp.format("%Y-%m-%d %H:%M:%S"),
            activity_kind_label(entry.kind),
            truncate_str(&entry.actor, 22),
            truncate_str(&entry.summary, 88)
        );
        if !entry.detail.is_empty() {
            let _ = write!(out, "  [{}]", truncate_str(&entry.detail, 48));
        }
        out.push('\n');
    }
    Ok(out)
}

fn format_mutation(action: &str, task: &Task) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{action} task {}", task.id);
    let _ = writeln!(out, "Status: {}", task_status_label(task));
    let _ = writeln!(out, "Version: {}", task.version);
    let _ = writeln!(out, "Assignee: {}", task.assignee);
    if !task.result.is_empty() {
        let _ = writeln!(out, "Result: {}", task.result);
    }
    if let Some(ts) = task.removed_at {
        let _ = writeln!(out, "Removed: {}", ts.format("%Y-%m-%d %H:%M:%S"));
        if !task.removed_by.is_empty() {
            let _ = writeln!(out, "Removed By: {}", task.removed_by);
        }
        if !task.remove_reason.is_empty() {
            let _ = writeln!(out, "Remove Reason: {}", task.remove_reason);
        }
    }
    out
}

fn format_intervention(resp: &ax_proto::responses::InterveneTaskResponse) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Intervened task {}", resp.task.id);
    let _ = writeln!(out, "Action: {}", resp.action);
    let _ = writeln!(out, "Status: {}", resp.status);
    let _ = writeln!(out, "Task Status: {}", task_status_label(&resp.task));
    let _ = writeln!(out, "Version: {}", resp.task.version);
    if !resp.message_id.is_empty() {
        let _ = writeln!(out, "Message ID: {}", resp.message_id);
    }
    if resp.action == "retry" {
        out.push_str(
            "Retry semantics: queued a standardized follow-up message on the same task ID.\n",
        );
    }
    out
}

// ---------- shared helpers ----------

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
    let elapsed = (Utc::now() - task.updated_at).num_seconds();
    elapsed >= task.stale_after_seconds
}

fn task_status_label(task: &Task) -> String {
    let base = task_status_str(&task.status);
    if task_is_stale(task)
        && !matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
    {
        format!("{base} stale")
    } else {
        base.to_owned()
    }
}

fn task_status_str(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn start_mode_label(mode: &TaskStartMode) -> &'static str {
    match mode {
        TaskStartMode::Default => "default",
        TaskStartMode::Fresh => "fresh",
    }
}

fn task_priority_label(priority: Option<&TaskPriority>) -> String {
    match priority {
        Some(TaskPriority::Urgent) => "urgent".into(),
        Some(TaskPriority::High) => "high".into(),
        Some(TaskPriority::Normal) | None => "normal".into(),
        Some(TaskPriority::Low) => "low".into(),
    }
}

fn task_priority_order(priority: Option<&TaskPriority>) -> i32 {
    match priority {
        Some(TaskPriority::Urgent) => 0,
        Some(TaskPriority::High) => 1,
        None | Some(TaskPriority::Normal) => 2,
        Some(TaskPriority::Low) => 3,
    }
}

fn task_sort_order(status: &TaskStatus) -> i32 {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Pending => 1,
        TaskStatus::Failed => 2,
        TaskStatus::Cancelled => 3,
        TaskStatus::Completed => 4,
        TaskStatus::Blocked => 5,
    }
}

fn sort_tasks_for_display(tasks: &mut [Task]) {
    tasks.sort_by(|a, b| {
        let oa = task_sort_order(&a.status);
        let ob = task_sort_order(&b.status);
        if oa != ob {
            return oa.cmp(&ob);
        }
        let pa = task_priority_order(a.priority.as_ref());
        let pb = task_priority_order(b.priority.as_ref());
        if pa != pb {
            return pa.cmp(&pb);
        }
        if a.updated_at != b.updated_at {
            return b.updated_at.cmp(&a.updated_at);
        }
        if a.created_at != b.created_at {
            return b.created_at.cmp(&a.created_at);
        }
        a.id.cmp(&b.id)
    });
}

fn task_age_seconds(task: &Task) -> i64 {
    (Utc::now() - task.updated_at).num_seconds().max(0)
}

fn format_task_age(task: &Task) -> String {
    format_age_seconds(task_age_seconds(task))
}

fn format_age_seconds(seconds: i64) -> String {
    let d = seconds.max(0);
    if d < 60 {
        format!("{d}s")
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86_400 {
        format!("{}h", d / 3600)
    } else {
        format!("{}d", d / 86_400)
    }
}

fn task_last_update_preview(task: &Task) -> String {
    if let Some(last) = task.logs.last() {
        return last.message.clone();
    }
    if !task.result.is_empty() {
        return task.result.clone();
    }
    if !task.description.is_empty() {
        return task.description.clone();
    }
    String::new()
}

fn task_operator_hint(task: &Task) -> String {
    if let Some(info) = &task.stale_info {
        if info.is_stale && !info.recommended_action.is_empty() {
            return info.recommended_action.clone();
        }
        if info.state_divergence {
            return info.state_divergence_note.clone();
        }
        if info.pending_messages > 0 {
            return format!("{} pending message(s) queued", info.pending_messages);
        }
    }
    let preview = task_last_update_preview(task);
    if preview.is_empty() {
        "awaiting next progress update".to_owned()
    } else {
        preview
    }
}

fn short_task_id(id: &str) -> String {
    if id.chars().count() > 8 {
        id.chars().take(8).collect()
    } else {
        id.to_owned()
    }
}

fn task_expected_version_arg(version: i64) -> String {
    if version <= 0 {
        String::new()
    } else {
        format!(" --expected-version {version}")
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

fn recent_task_logs(task: &Task, limit: usize) -> &[TaskLog] {
    if limit == 0 || task.logs.len() <= limit {
        return &task.logs;
    }
    &task.logs[task.logs.len() - limit..]
}

fn related_task_messages<'a>(
    task: &Task,
    history: &'a [HistoryEntry],
    limit: usize,
) -> Vec<&'a HistoryEntry> {
    if limit == 0 {
        // limit=0 in activity path → include all matching
        let mut related: Vec<&HistoryEntry> = history
            .iter()
            .filter(|entry| is_related_history(entry, task))
            .collect();
        related.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        return related;
    }
    let mut related: Vec<&HistoryEntry> = Vec::with_capacity(limit);
    for entry in history.iter().rev() {
        if is_related_history(entry, task) {
            related.push(entry);
            if related.len() == limit {
                break;
            }
        }
    }
    related.reverse();
    related
}

fn is_related_history(entry: &HistoryEntry, task: &Task) -> bool {
    entry.task_id == task.id
        || entry.content.contains(&task.id)
        || entry.from == task.assignee
        || entry.to == task.assignee
        || entry.from == task.created_by
        || entry.to == task.created_by
}

// ---------- task activity ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TaskActivityKind {
    Lifecycle,
    Log,
    Message,
}

#[derive(Debug, Clone)]
struct TaskActivityEntry {
    timestamp: DateTime<Utc>,
    kind: TaskActivityKind,
    actor: String,
    summary: String,
    detail: String,
}

fn activity_kind_label(kind: TaskActivityKind) -> &'static str {
    match kind {
        TaskActivityKind::Lifecycle => "lifecycle",
        TaskActivityKind::Log => "log",
        TaskActivityKind::Message => "message",
    }
}

fn build_task_activity(
    task: &Task,
    history: &[HistoryEntry],
    limit: usize,
) -> Vec<TaskActivityEntry> {
    let mut entries: Vec<TaskActivityEntry> = Vec::new();
    entries.push(TaskActivityEntry {
        timestamp: task.created_at,
        kind: TaskActivityKind::Lifecycle,
        actor: task.created_by.clone(),
        summary: format!("created task for {}", task.assignee),
        detail: task.description.clone(),
    });

    if task.status == TaskStatus::Completed && !task.result.is_empty() {
        entries.push(TaskActivityEntry {
            timestamp: task.updated_at,
            kind: TaskActivityKind::Lifecycle,
            actor: task.assignee.clone(),
            summary: "completed task".into(),
            detail: task.result.clone(),
        });
    } else if task.status == TaskStatus::Failed && !task.result.is_empty() {
        entries.push(TaskActivityEntry {
            timestamp: task.updated_at,
            kind: TaskActivityKind::Lifecycle,
            actor: task.assignee.clone(),
            summary: "failed task".into(),
            detail: task.result.clone(),
        });
    } else if task.status == TaskStatus::Cancelled && !task.result.is_empty() {
        entries.push(TaskActivityEntry {
            timestamp: task.updated_at,
            kind: TaskActivityKind::Lifecycle,
            actor: task.assignee.clone(),
            summary: "cancelled task".into(),
            detail: task.result.clone(),
        });
    } else if task.status == TaskStatus::InProgress {
        entries.push(TaskActivityEntry {
            timestamp: task.updated_at,
            kind: TaskActivityKind::Lifecycle,
            actor: task.assignee.clone(),
            summary: "task in progress".into(),
            detail: String::new(),
        });
    }

    if let Some(ts) = task.removed_at {
        let actor = if task.removed_by.is_empty() {
            task.created_by.clone()
        } else {
            task.removed_by.clone()
        };
        entries.push(TaskActivityEntry {
            timestamp: ts,
            kind: TaskActivityKind::Lifecycle,
            actor,
            summary: "removed task".into(),
            detail: task.remove_reason.clone(),
        });
    }

    for log in &task.logs {
        entries.push(TaskActivityEntry {
            timestamp: log.timestamp,
            kind: TaskActivityKind::Log,
            actor: log.workspace.clone(),
            summary: log.message.clone(),
            detail: String::new(),
        });
    }

    for msg in related_task_messages(task, history, 0) {
        let mut summary = msg.content.replace('\n', " ");
        if summary.contains(&task.id) {
            summary = summary.replace(&task.id, &short_task_id(&task.id));
        }
        entries.push(TaskActivityEntry {
            timestamp: msg.timestamp,
            kind: TaskActivityKind::Message,
            actor: format!("{}->{}", msg.from, msg.to),
            summary,
            detail: String::new(),
        });
    }

    entries.sort_by(|a, b| {
        if a.timestamp == b.timestamp {
            a.kind.cmp(&b.kind)
        } else {
            a.timestamp.cmp(&b.timestamp)
        }
    });

    if limit > 0 && entries.len() > limit {
        let drop = entries.len() - limit;
        entries.drain(..drop);
    }
    entries
}

// ---------- recover preview ----------

fn task_recovery_preview_lines(task: &Task) -> Vec<String> {
    let mut lines = vec![
        format!("Task: {}", task.title),
        format!("ID: {}", task.id),
        format!("Status: {}", task_status_label(task)),
        format!("Version: {}", task.version),
        format!("Assignee: {}", task.assignee),
        format!("Created By: {}", task.created_by),
        format!("Updated: {} ago", format_task_age(task)),
    ];
    if let Some(ts) = task.removed_at {
        lines.push(format!(
            "Removed: {} by {}",
            ts.format("%Y-%m-%d %H:%M:%S"),
            truncate_str(&task.removed_by, 24)
        ));
        if !task.remove_reason.is_empty() {
            lines.push(format!("Remove Reason: {}", task.remove_reason));
        }
        lines.push(String::new());
        lines.push("Semantics:".into());
        lines.push(
            "- recover is preview-only and this task is already archived/removed from list results"
                .into(),
        );
        return lines;
    }
    if let Some(info) = &task.stale_info {
        lines.push(String::new());
        lines.push("Signals:".into());
        if !info.reason.is_empty() {
            lines.push(format!("- reason: {}", info.reason));
        }
        if !info.recommended_action.is_empty() {
            lines.push(format!("- daemon hint: {}", info.recommended_action));
        }
        if info.pending_messages > 0 {
            lines.push(format!("- pending_messages: {}", info.pending_messages));
        }
        if info.wake_pending {
            let mut wake = format!("- wake_retry: attempt {} pending", info.wake_attempts);
            if let Some(ts) = info.next_wake_retry_at {
                wake.push_str(" until ");
                wake.push_str(&ts.format("%Y-%m-%d %H:%M:%S").to_string());
            }
            lines.push(wake);
        }
        if info.state_divergence {
            lines.push(format!("- divergence: {}", info.state_divergence_note));
        }
    }

    lines.push(String::new());
    lines.push("Semantics:".into());
    lines.push(
        "- recover is preview-only; use intervene/retry/cancel/remove to mutate the task".into(),
    );
    let version_arg = task_expected_version_arg(task.version);
    if matches!(task.status, TaskStatus::Pending | TaskStatus::InProgress) {
        lines.push(String::new());
        lines.push("Next steps:".into());
        lines.push(format!(
            "- ax tasks intervene {} --action wake{version_arg}",
            task.id
        ));
        lines.push(format!(
            "- ax tasks intervene {} --action interrupt{version_arg}",
            task.id
        ));
        lines.push(format!("- ax tasks retry {}{version_arg}", task.id));
        lines.push("  retry queues a standardized follow-up message on the same task ID".into());
        lines.push(format!("- ax tasks cancel {}{version_arg}", task.id));
        return lines;
    }

    lines.push(String::new());
    lines.push("Next steps:".into());
    lines.push("- task is terminal; intervene/retry is unavailable".into());
    if matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    ) {
        lines.push(format!("- ax tasks remove {}{version_arg}", task.id));
    }
    lines
}

// ---------- history file ----------

const HISTORY_FILE_NAME: &str = "message_history.jsonl";

fn history_file_path(socket_path: &Path) -> PathBuf {
    let expanded = expand_socket_path(&socket_path.display().to_string());
    expanded
        .parent()
        .map_or_else(|| PathBuf::from(HISTORY_FILE_NAME), Path::to_path_buf)
        .join(HISTORY_FILE_NAME)
}

fn read_history_file(path: &Path, max_entries: usize) -> Vec<HistoryEntry> {
    let Ok(data) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut entries: Vec<HistoryEntry> = Vec::new();
    for line in data.lines() {
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) {
            entries.push(entry);
        }
    }
    if entries.len() > max_entries {
        entries.drain(..entries.len() - max_entries);
    }
    entries
}

/// Shared by parsers: accept Go's `--status` values.
pub(crate) fn parse_task_status_flag(raw: &str) -> Result<Option<TaskStatus>, TasksError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed {
        "pending" => Ok(Some(TaskStatus::Pending)),
        "in_progress" => Ok(Some(TaskStatus::InProgress)),
        "completed" => Ok(Some(TaskStatus::Completed)),
        "failed" => Ok(Some(TaskStatus::Failed)),
        "cancelled" => Ok(Some(TaskStatus::Cancelled)),
        other => Err(TasksError::InvalidStatus(other.to_owned())),
    }
}

/// Public surface for parsers/tests in [`crate::tasks::filter`].
#[allow(dead_code)]
pub(crate) fn filter_mode_label(mode: TaskFilterMode) -> &'static str {
    match mode {
        TaskFilterMode::Active => "active",
        TaskFilterMode::Stale => "stale",
        TaskFilterMode::Done => "done",
        TaskFilterMode::All => "all",
    }
}

#[derive(Debug)]
pub(crate) enum TasksError {
    Connect(DaemonClientError),
    Request(DaemonClientError),
    InvalidStatus(String),
    InvalidAction(String),
}

impl std::fmt::Display for TasksError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "connect to daemon: {e} (is daemon running?)"),
            Self::Request(e) => write!(f, "{e}"),
            Self::InvalidStatus(s) => write!(f, "invalid --status {s:?}"),
            Self::InvalidAction(s) => {
                write!(
                    f,
                    "invalid --action {s:?} (must be wake, interrupt, or retry)"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_task(id: &str, status: TaskStatus, priority: Option<TaskPriority>) -> Task {
        let now = Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            assignee: "alpha".into(),
            created_by: "orch".into(),
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
            stale_info: None,
            removed_at: None,
            removed_by: String::new(),
            remove_reason: String::new(),
            created_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            updated_at: now,
        }
    }

    #[test]
    fn status_label_adds_stale_for_active_tasks_with_stale_info() {
        let mut task = make_task("abc", TaskStatus::InProgress, None);
        task.stale_info = Some(ax_proto::types::TaskStaleInfo {
            is_stale: true,
            reason: String::new(),
            recommended_action: String::new(),
            last_progress_at: Utc::now(),
            age_seconds: 0,
            pending_messages: 0,
            last_message_at: None,
            wake_pending: false,
            wake_attempts: 0,
            next_wake_retry_at: None,
            claim_state: String::new(),
            claim_state_note: String::new(),
            runnable: false,
            runnable_reason: String::new(),
            recovery_eligible: false,
            state_divergence: false,
            state_divergence_note: String::new(),
        });
        assert_eq!(task_status_label(&task), "in_progress stale");

        task.status = TaskStatus::Completed;
        // Terminal tasks never earn the stale suffix.
        assert_eq!(task_status_label(&task), "completed");
    }

    #[test]
    fn priority_label_uses_normal_as_default() {
        assert_eq!(task_priority_label(None), "normal");
        assert_eq!(task_priority_label(Some(&TaskPriority::Urgent)), "urgent");
    }

    #[test]
    fn sort_tasks_groups_by_status_then_priority_then_recency() {
        let mut a = make_task("a", TaskStatus::Completed, None);
        a.updated_at = Utc.timestamp_opt(1_700_000_001, 0).single().unwrap();
        let mut b = make_task("b", TaskStatus::InProgress, Some(TaskPriority::Normal));
        b.updated_at = Utc.timestamp_opt(1_700_000_002, 0).single().unwrap();
        let mut c = make_task("c", TaskStatus::InProgress, Some(TaskPriority::Urgent));
        c.updated_at = Utc.timestamp_opt(1_700_000_003, 0).single().unwrap();
        let mut d = make_task("d", TaskStatus::Pending, None);
        d.updated_at = Utc.timestamp_opt(1_700_000_004, 0).single().unwrap();

        let mut tasks = vec![a.clone(), b.clone(), c.clone(), d.clone()];
        sort_tasks_for_display(&mut tasks);
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "b", "d", "a"]);
    }

    #[test]
    fn task_recovery_preview_mentions_intervene_options_for_active_task() {
        let task = make_task("task123", TaskStatus::InProgress, None);
        let lines = task_recovery_preview_lines(&task);
        let blob = lines.join("\n");
        assert!(blob.contains("ax tasks intervene task123 --action wake"));
        assert!(blob.contains("ax tasks retry task123"));
    }

    #[test]
    fn task_recovery_preview_for_terminal_task_suggests_remove() {
        let task = make_task("task123", TaskStatus::Completed, None);
        let blob = task_recovery_preview_lines(&task).join("\n");
        assert!(blob.contains("task is terminal"));
        assert!(blob.contains("ax tasks remove task123"));
    }

    #[test]
    fn related_task_messages_filters_by_task_id_and_actors() {
        let task = make_task("task123", TaskStatus::InProgress, None);
        let now = Utc::now();
        let history = vec![
            HistoryEntry {
                timestamp: now,
                from: "alpha".into(),
                to: "orch".into(),
                content: "hi".into(),
                task_id: String::new(),
            },
            HistoryEntry {
                timestamp: now,
                from: "unrelated".into(),
                to: "somebody".into(),
                content: "nothing here".into(),
                task_id: String::new(),
            },
            HistoryEntry {
                timestamp: now,
                from: "orch".into(),
                to: "alpha".into(),
                content: "mentions task123 somewhere".into(),
                task_id: String::new(),
            },
        ];
        let related = related_task_messages(&task, &history, 5);
        assert_eq!(related.len(), 2);
    }
}
