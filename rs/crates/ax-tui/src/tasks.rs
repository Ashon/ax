//! Tasks stream view — port of the single-column slice of
//! `cmd/watch_streams.go::renderTasks`. Reads `tasks.json` from the
//! daemon state dir and renders a summary header + table in the
//! body pane.
//!
//! The Go watch TUI has a richer split layout (task list + detail
//! pane) plus a filter-cycle; those land in follow-up slices so this
//! module stays compact. Helpers here are close relatives of the
//! ones in `ax-cli::tasks`; a future slice can extract them into a
//! shared crate once the cross-crate usage stabilises.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ax_daemon::expand_socket_path;
use ax_proto::types::{Task, TaskPriority, TaskStatus};
use chrono::Utc;

const TASKS_FILE_NAME: &str = "tasks.json";

/// Resolve the tasks snapshot path from whatever the user passed
/// for `--socket`.
pub(crate) fn tasks_file_path(socket_path: &Path) -> PathBuf {
    let expanded = expand_socket_path(&socket_path.display().to_string());
    expanded
        .parent()
        .map_or_else(|| PathBuf::from(TASKS_FILE_NAME), Path::to_path_buf)
        .join(TASKS_FILE_NAME)
}

/// Parse the tasks snapshot, returning an empty slice when the file
/// is missing or malformed. Tasks are sorted in display order so
/// rendering can iterate them directly.
pub(crate) fn read_tasks(path: &Path) -> Vec<Task> {
    let Ok(data) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(mut tasks) = serde_json::from_str::<Vec<Task>>(&data) else {
        return Vec::new();
    };
    sort_tasks_for_display(&mut tasks);
    tasks
}

/// Low-cardinality counts the header uses. Mirrors
/// `cmd/task_helpers.go::taskSummary` minus the
/// `top_attention_ids` pass (easy to add later if the TUI grows an
/// attention badge).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TaskSummary {
    pub total: usize,
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub stale: usize,
    pub diverged: usize,
    pub queued_messages: i64,
    pub urgent_or_high: usize,
}

pub(crate) fn summarize_tasks(tasks: &[Task]) -> TaskSummary {
    let mut s = TaskSummary {
        total: tasks.len(),
        ..TaskSummary::default()
    };
    for task in tasks {
        match task.status {
            TaskStatus::Pending => s.pending += 1,
            TaskStatus::InProgress => s.in_progress += 1,
            TaskStatus::Completed => s.completed += 1,
            TaskStatus::Failed => s.failed += 1,
            TaskStatus::Cancelled => s.cancelled += 1,
            TaskStatus::Blocked => {}
        }
        if matches!(
            task.priority,
            Some(TaskPriority::Urgent | TaskPriority::High)
        ) {
            s.urgent_or_high += 1;
        }
        if task_is_stale(task) {
            s.stale += 1;
        }
        if let Some(info) = &task.stale_info {
            s.queued_messages += info.pending_messages;
            if info.state_divergence {
                s.diverged += 1;
            }
        }
    }
    s
}

pub(crate) fn format_task_summary(s: &TaskSummary) -> String {
    let mut out = format!(
        "total={}  pending={}  in_progress={}  stale={}  diverged={}  queued_msgs={}",
        s.total, s.pending, s.in_progress, s.stale, s.diverged, s.queued_messages,
    );
    if s.cancelled > 0 {
        let _ = write!(out, "  cancelled={}", s.cancelled);
    }
    if s.urgent_or_high > 0 {
        let _ = write!(out, "  high_pri={}", s.urgent_or_high);
    }
    out
}

pub(crate) fn task_is_stale(task: &Task) -> bool {
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
    (Utc::now() - task.updated_at).num_seconds() >= task.stale_after_seconds
}

pub(crate) fn task_status_label(task: &Task) -> String {
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

pub(crate) fn task_priority_label(priority: Option<&TaskPriority>) -> &'static str {
    match priority {
        Some(TaskPriority::Urgent) => "urgent",
        Some(TaskPriority::High) => "high",
        Some(TaskPriority::Low) => "low",
        Some(TaskPriority::Normal) | None => "normal",
    }
}

pub(crate) fn task_priority_order(priority: Option<&TaskPriority>) -> i32 {
    match priority {
        Some(TaskPriority::Urgent) => 0,
        Some(TaskPriority::High) => 1,
        None | Some(TaskPriority::Normal) => 2,
        Some(TaskPriority::Low) => 3,
    }
}

pub(crate) fn task_sort_order(status: &TaskStatus) -> i32 {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Pending => 1,
        TaskStatus::Failed => 2,
        TaskStatus::Cancelled => 3,
        TaskStatus::Completed => 4,
        TaskStatus::Blocked => 5,
    }
}

pub(crate) fn sort_tasks_for_display(tasks: &mut [Task]) {
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

pub(crate) fn short_task_id(id: &str) -> String {
    if id.chars().count() > 8 {
        id.chars().take(8).collect()
    } else {
        id.to_owned()
    }
}

pub(crate) fn format_task_age(task: &Task) -> String {
    let secs = (Utc::now() - task.updated_at).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

pub(crate) fn truncate(s: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= limit {
        return s.to_owned();
    }
    if limit == 1 {
        return "…".to_owned();
    }
    let mut out: String = chars[..limit - 1].iter().collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ax_proto::types::{TaskStartMode, TaskStatus};
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn task(id: &str, status: TaskStatus, priority: Option<TaskPriority>) -> Task {
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
            start_mode: TaskStartMode::Default,
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
    fn read_tasks_returns_empty_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let got = read_tasks(&tmp.path().join("nope.json"));
        assert!(got.is_empty());
    }

    #[test]
    fn read_tasks_parses_and_sorts_array_payload() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("tasks.json");
        let tasks = vec![
            task("a", TaskStatus::Completed, None),
            task("b", TaskStatus::InProgress, Some(TaskPriority::Urgent)),
        ];
        std::fs::write(&path, serde_json::to_string(&tasks).unwrap()).unwrap();
        let got = read_tasks(&path);
        assert_eq!(got.len(), 2);
        // sort places InProgress before Completed.
        assert_eq!(got[0].id, "b");
        assert_eq!(got[1].id, "a");
    }

    #[test]
    fn summarize_counts_by_status_and_priority() {
        let tasks = vec![
            task("a", TaskStatus::Pending, Some(TaskPriority::High)),
            task("b", TaskStatus::InProgress, None),
            task("c", TaskStatus::Completed, None),
        ];
        let s = summarize_tasks(&tasks);
        assert_eq!(s.total, 3);
        assert_eq!(s.pending, 1);
        assert_eq!(s.in_progress, 1);
        assert_eq!(s.completed, 1);
        assert_eq!(s.urgent_or_high, 1);
    }

    #[test]
    fn format_task_summary_emits_canonical_shape() {
        let s = TaskSummary {
            total: 3,
            pending: 1,
            in_progress: 1,
            completed: 1,
            urgent_or_high: 1,
            ..TaskSummary::default()
        };
        assert_eq!(
            format_task_summary(&s),
            "total=3  pending=1  in_progress=1  stale=0  diverged=0  queued_msgs=0  high_pri=1"
        );
    }

    #[test]
    fn truncate_adds_ellipsis_past_budget() {
        assert_eq!(truncate("hello", 3), "he…");
        assert_eq!(truncate("hi", 5), "hi");
        assert_eq!(truncate("hello", 0), "");
    }

    #[test]
    fn short_task_id_clips_at_8_chars() {
        assert_eq!(short_task_id("01234567"), "01234567");
        assert_eq!(short_task_id("0123456789"), "01234567");
    }
}
