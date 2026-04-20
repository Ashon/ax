//! Tasks stream view — reads `tasks-state.json` from the daemon state dir
//! and renders a summary header + table in the body pane.
//!
//! Helpers here are close relatives of the ones in `ax-cli::tasks`;
//! a future slice can extract them into a shared crate once the
//! cross-crate usage stabilises.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ax_daemon::{expand_socket_path, HistoryEntry};
use ax_proto::types::{Task, TaskPriority, TaskStatus};
use chrono::{DateTime, Utc};

const TASKS_FILE_NAME: &str = "tasks-state.json";
const LEGACY_TASKS_FILE_NAME: &str = "tasks.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SnapshotRead<T> {
    Loaded(T),
    Missing,
    Error(String),
}

/// Cycle order for the `f` key: active → stale → done → all → active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskFilterMode {
    Active,
    Stale,
    Done,
    All,
}

impl TaskFilterMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Done => "done",
            Self::All => "all",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Active => Self::Stale,
            Self::Stale => Self::Done,
            Self::Done => Self::All,
            Self::All => Self::Active,
        }
    }
}

/// Filter `tasks` according to `mode` + re-sort the result so the
/// caller gets a display-ready slice. Non-allocating for `All`.
pub(crate) fn filter_tasks(tasks: &[Task], mode: TaskFilterMode) -> Vec<Task> {
    let mut out: Vec<Task> = tasks
        .iter()
        .filter(|task| match mode {
            TaskFilterMode::Active => {
                matches!(
                    task.status,
                    TaskStatus::Pending | TaskStatus::InProgress | TaskStatus::Blocked
                )
            }
            TaskFilterMode::Stale => task_is_stale(task),
            TaskFilterMode::Done => matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
            ),
            TaskFilterMode::All => true,
        })
        .cloned()
        .collect();
    sort_tasks_for_display(&mut out);
    out
}

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
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn read_tasks(path: &Path) -> Vec<Task> {
    match read_tasks_snapshot(path) {
        SnapshotRead::Loaded(tasks) => tasks,
        SnapshotRead::Missing | SnapshotRead::Error(_) => Vec::new(),
    }
}

pub(crate) fn read_tasks_snapshot(path: &Path) -> SnapshotRead<Vec<Task>> {
    match read_tasks_snapshot_file(path) {
        SnapshotRead::Missing if path.file_name().is_some_and(|name| name == TASKS_FILE_NAME) => {
            read_tasks_snapshot_file(&path.with_file_name(LEGACY_TASKS_FILE_NAME))
        }
        result => result,
    }
}

fn read_tasks_snapshot_file(path: &Path) -> SnapshotRead<Vec<Task>> {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SnapshotRead::Missing,
        Err(e) => {
            return SnapshotRead::Error(format!("tasks snapshot read {}: {e}", path.display()));
        }
    };
    let mut tasks = match serde_json::from_str::<Vec<Task>>(&data) {
        Ok(tasks) => tasks,
        Err(e) => {
            return SnapshotRead::Error(format!("tasks snapshot parse {}: {e}", path.display()));
        }
    };
    sort_tasks_for_display(&mut tasks);
    SnapshotRead::Loaded(tasks)
}

/// Low-cardinality counts the header uses. `top_attention_ids` is
/// intentionally omitted — easy to add later if the TUI grows an
/// attention badge.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TaskSummary {
    pub total: usize,
    pub pending: usize,
    pub in_progress: usize,
    pub blocked: usize,
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
            TaskStatus::Blocked => s.blocked += 1,
            TaskStatus::Completed => s.completed += 1,
            TaskStatus::Failed => s.failed += 1,
            TaskStatus::Cancelled => s.cancelled += 1,
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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn format_task_summary(s: &TaskSummary) -> String {
    let mut out = format!(
        "total={}  pending={}  in_progress={}  blocked={}  stale={}  diverged={}  queued_msgs={}",
        s.total, s.pending, s.in_progress, s.blocked, s.stale, s.diverged, s.queued_messages,
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
        TaskStatus::Blocked => 2,
        TaskStatus::Failed => 3,
        TaskStatus::Cancelled => 4,
        TaskStatus::Completed => 5,
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

/// One entry in the task-detail activity timeline. Ordering is by
/// timestamp ascending, then kind (lifecycle < log < message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskActivityEntry {
    pub timestamp: DateTime<Utc>,
    pub kind: TaskActivityKind,
    pub actor: String,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TaskActivityKind {
    Lifecycle,
    Log,
    Message,
}

impl TaskActivityKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Lifecycle => "lifecycle",
            Self::Log => "log",
            Self::Message => "message",
        }
    }
}

/// Build the activity timeline for a task: lifecycle events
/// derived from the task's own state, its log entries, and any
/// related history messages. Caller may pass `limit=0` to skip
/// trimming; otherwise the oldest entries drop out first so the
/// tail (most recent) fits.
pub(crate) fn build_task_activity(
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
    });

    match task.status {
        TaskStatus::Completed if !task.result.is_empty() => {
            entries.push(TaskActivityEntry {
                timestamp: task.updated_at,
                kind: TaskActivityKind::Lifecycle,
                actor: task.assignee.clone(),
                summary: "completed task".into(),
            });
        }
        TaskStatus::Failed if !task.result.is_empty() => {
            entries.push(TaskActivityEntry {
                timestamp: task.updated_at,
                kind: TaskActivityKind::Lifecycle,
                actor: task.assignee.clone(),
                summary: "failed task".into(),
            });
        }
        TaskStatus::Cancelled if !task.result.is_empty() => {
            entries.push(TaskActivityEntry {
                timestamp: task.updated_at,
                kind: TaskActivityKind::Lifecycle,
                actor: task.assignee.clone(),
                summary: "cancelled task".into(),
            });
        }
        TaskStatus::InProgress => {
            entries.push(TaskActivityEntry {
                timestamp: task.updated_at,
                kind: TaskActivityKind::Lifecycle,
                actor: task.assignee.clone(),
                summary: "task in progress".into(),
            });
        }
        _ => {}
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
        });
    }

    for log in &task.logs {
        entries.push(TaskActivityEntry {
            timestamp: log.timestamp,
            kind: TaskActivityKind::Log,
            actor: log.workspace.clone(),
            summary: log.message.clone(),
        });
    }

    for msg in related_task_messages(task, history) {
        let mut summary = msg.content.replace('\n', " ");
        if summary.contains(&task.id) {
            let short = short_task_id(&task.id);
            summary = summary.replace(&task.id, &short);
        }
        entries.push(TaskActivityEntry {
            timestamp: msg.timestamp,
            kind: TaskActivityKind::Message,
            actor: format!("{}->{}", msg.from, msg.to),
            summary,
        });
    }

    entries.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.kind.cmp(&b.kind))
    });
    if limit > 0 && entries.len() > limit {
        let drop = entries.len() - limit;
        entries.drain(..drop);
    }
    entries
}

fn related_task_messages<'a>(task: &Task, history: &'a [HistoryEntry]) -> Vec<&'a HistoryEntry> {
    history
        .iter()
        .filter(|entry| {
            entry.task_id == task.id
                || entry.content.contains(&task.id)
                || entry.from == task.assignee
                || entry.to == task.assignee
                || entry.from == task.created_by
                || entry.to == task.created_by
        })
        .collect()
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
    use std::ffi::OsStr;
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
        let path = tmp.path().join(TASKS_FILE_NAME);
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
    fn tasks_file_path_uses_primary_state_snapshot_name() {
        let tmp = TempDir::new().unwrap();
        let path = tasks_file_path(&tmp.path().join("daemon.sock"));
        assert_eq!(path.file_name(), Some(OsStr::new(TASKS_FILE_NAME)));
    }

    #[test]
    fn read_tasks_snapshot_prefers_primary_state_file_over_legacy_file() {
        let tmp = TempDir::new().unwrap();
        let primary = tmp.path().join(TASKS_FILE_NAME);
        let legacy = tmp.path().join(LEGACY_TASKS_FILE_NAME);
        std::fs::write(
            &primary,
            serde_json::to_string(&vec![task("primary", TaskStatus::Pending, None)]).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &legacy,
            serde_json::to_string(&vec![task("legacy", TaskStatus::Pending, None)]).unwrap(),
        )
        .unwrap();

        let got = read_tasks_snapshot(&primary);
        assert!(matches!(got, SnapshotRead::Loaded(tasks) if tasks[0].id == "primary"));
    }

    #[test]
    fn read_tasks_snapshot_falls_back_to_legacy_file_when_primary_is_missing() {
        let tmp = TempDir::new().unwrap();
        let primary = tmp.path().join(TASKS_FILE_NAME);
        let legacy = tmp.path().join(LEGACY_TASKS_FILE_NAME);
        std::fs::write(
            &legacy,
            serde_json::to_string(&vec![task("legacy", TaskStatus::Pending, None)]).unwrap(),
        )
        .unwrap();

        let got = read_tasks_snapshot(&primary);
        assert!(matches!(got, SnapshotRead::Loaded(tasks) if tasks[0].id == "legacy"));
    }

    #[test]
    fn read_tasks_snapshot_reports_missing_when_primary_and_legacy_are_missing() {
        let tmp = TempDir::new().unwrap();
        let primary = tmp.path().join(TASKS_FILE_NAME);
        assert!(matches!(
            read_tasks_snapshot(&primary),
            SnapshotRead::Missing
        ));
    }

    #[test]
    fn read_tasks_snapshot_distinguishes_missing_and_malformed_files() {
        let tmp = TempDir::new().unwrap();
        assert!(matches!(
            read_tasks_snapshot(&tmp.path().join("missing.json")),
            SnapshotRead::Missing
        ));

        let path = tmp.path().join("tasks.json");
        std::fs::write(&path, "not json").unwrap();
        let got = read_tasks_snapshot(&path);
        assert!(matches!(got, SnapshotRead::Error(message) if message.contains("parse")));
    }

    #[test]
    fn summarize_counts_by_status_and_priority() {
        let tasks = vec![
            task("a", TaskStatus::Pending, Some(TaskPriority::High)),
            task("b", TaskStatus::InProgress, None),
            task("c", TaskStatus::Completed, None),
            task("d", TaskStatus::Blocked, Some(TaskPriority::Urgent)),
        ];
        let s = summarize_tasks(&tasks);
        assert_eq!(s.total, 4);
        assert_eq!(s.pending, 1);
        assert_eq!(s.in_progress, 1);
        assert_eq!(s.blocked, 1);
        assert_eq!(s.completed, 1);
        assert_eq!(s.urgent_or_high, 2);
    }

    #[test]
    fn format_task_summary_emits_canonical_shape() {
        let s = TaskSummary {
            total: 3,
            pending: 1,
            in_progress: 1,
            blocked: 1,
            completed: 1,
            urgent_or_high: 1,
            ..TaskSummary::default()
        };
        assert_eq!(
            format_task_summary(&s),
            "total=3  pending=1  in_progress=1  blocked=1  stale=0  diverged=0  queued_msgs=0  high_pri=1"
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

    #[test]
    fn filter_mode_cycles_active_stale_done_all() {
        assert_eq!(TaskFilterMode::Active.next(), TaskFilterMode::Stale);
        assert_eq!(TaskFilterMode::Stale.next(), TaskFilterMode::Done);
        assert_eq!(TaskFilterMode::Done.next(), TaskFilterMode::All);
        assert_eq!(TaskFilterMode::All.next(), TaskFilterMode::Active);
    }

    #[test]
    fn filter_tasks_returns_only_active_by_default() {
        let tasks = vec![
            task("a", TaskStatus::Pending, None),
            task("b", TaskStatus::Completed, None),
            task("c", TaskStatus::Failed, None),
            task("d", TaskStatus::Blocked, None),
        ];
        assert_eq!(
            filter_tasks(&tasks, TaskFilterMode::Active)
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>(),
            vec!["a".to_owned(), "d".to_owned()]
        );
        assert_eq!(
            filter_tasks(&tasks, TaskFilterMode::Done)
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>(),
            vec!["c".to_owned(), "b".to_owned()]
        );
        assert_eq!(filter_tasks(&tasks, TaskFilterMode::All).len(), 4);
    }

    #[test]
    fn sort_tasks_places_blocked_before_terminal_states() {
        let mut tasks = vec![
            task("done", TaskStatus::Completed, None),
            task("blocked", TaskStatus::Blocked, None),
            task("failed", TaskStatus::Failed, None),
            task("pending", TaskStatus::Pending, None),
        ];
        sort_tasks_for_display(&mut tasks);
        assert_eq!(
            tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["pending", "blocked", "failed", "done"]
        );
    }

    #[test]
    fn build_task_activity_joins_logs_and_related_messages() {
        let mut t = task("abc", TaskStatus::InProgress, None);
        t.logs.push(ax_proto::types::TaskLog {
            timestamp: Utc.timestamp_opt(1_700_000_500, 0).single().unwrap(),
            workspace: "alpha".into(),
            message: "started".into(),
        });
        let history = vec![
            HistoryEntry {
                timestamp: Utc.timestamp_opt(1_700_000_600, 0).single().unwrap(),
                from: "orch".into(),
                to: "alpha".into(),
                content: "please do abc".into(),
                task_id: String::new(),
            },
            HistoryEntry {
                timestamp: Utc.timestamp_opt(1_700_000_700, 0).single().unwrap(),
                from: "unrelated".into(),
                to: "other".into(),
                content: "nothing here".into(),
                task_id: String::new(),
            },
        ];
        let entries = build_task_activity(&t, &history, 0);
        // created_task + log(started) + in-progress lifecycle + 1 related msg
        assert_eq!(entries.len(), 4);
        // sorted ascending by timestamp
        for pair in entries.windows(2) {
            assert!(pair[0].timestamp <= pair[1].timestamp);
        }
        assert!(entries.iter().any(|e| e.kind == TaskActivityKind::Message));
    }

    #[test]
    fn build_task_activity_limit_drops_oldest_entries_first() {
        // Pending status has no extra lifecycle entry beyond "created
        // task", so we can assert that the limited tail preserves the
        // newest logs without racing against `updated_at = Utc::now()`.
        let mut t = task("abc", TaskStatus::Pending, None);
        t.created_at = Utc.timestamp_opt(1_699_999_900, 0).single().unwrap();
        t.updated_at = t.created_at;
        for i in 0..5 {
            t.logs.push(ax_proto::types::TaskLog {
                timestamp: Utc.timestamp_opt(1_700_000_000 + i, 0).single().unwrap(),
                workspace: "alpha".into(),
                message: format!("log {i}"),
            });
        }
        let entries = build_task_activity(&t, &[], 3);
        assert_eq!(entries.len(), 3);
        assert!(entries.last().unwrap().summary.contains("log 4"));
    }
}
