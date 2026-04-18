//! Utility helpers shared between the task store, the message queue
//! and task-handling envelope dispatch. Mirrors the free-standing
//! helpers in `internal/daemon/{taskref.go,daemon.go,daemon_handlers.go}`.

use std::sync::LazyLock;
use std::time::Duration;

use chrono::Utc;
use regex::Regex;

use ax_proto::types::{Message, Task, TaskPriority, TaskStartMode, TaskStatus, TaskWorkflowMode};

/// Duplicate status/log messages from the same workspace are suppressed
/// when they repeat a no-op update within this window. Mirrors
/// `internal/daemon/daemon.go::duplicateSuppressionWindow`.
pub(crate) const DUPLICATE_SUPPRESSION_WINDOW: Duration = Duration::from_secs(15);

/// `_cli` is the synthetic workspace name the CLI uses when sending
/// operator-facing messages. `validate_task_control` uses it to let the
/// operator override task ownership without hiding it as a separate
/// literal in every call site.
pub(crate) const OPERATOR_WORKSPACE_NAME: &str = "_cli";

static TASK_ID_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Go: (?i)task id:\s*([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})
    Regex::new(
        r"(?i)task id:\s*([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})",
    )
    .expect("task id regex must compile")
});

static DUPLICATE_NOOP_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Go's pattern with `\b` word boundaries and case-insensitive matching.
    Regex::new(
        r"(?i)\b(ack|acked|acknowledged|received|noted|thanks?|thank you|roger|copy that|working on it|on it|looking into it|in progress|still working|status|no update|no-op|noop|same update|same status)\b",
    )
    .expect("duplicate no-op regex must compile")
});

/// Pull a `Task ID: <uuid>` reference out of a free-form message body.
/// Returns an empty string when no match is found, matching Go.
#[must_use]
pub(crate) fn extract_task_id(content: &str) -> String {
    TASK_ID_PATTERN
        .captures(content)
        .and_then(|caps| caps.get(1))
        .map_or_else(String::new, |m| m.as_str().to_owned())
}

/// Normalise a message body for suppression comparison: trim, collapse
/// whitespace, lowercase. Mirrors `normalizeMessageForSuppression` in
/// `daemon.go`.
#[must_use]
pub(crate) fn normalize_message_for_suppression(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut last_ws = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !last_ws {
                out.push(' ');
                last_ws = true;
            }
        } else {
            out.extend(ch.to_lowercase());
            last_ws = false;
        }
    }
    out
}

/// Matches the "noop-style" status updates that the daemon treats as
/// silent when repeated. Input must already be normalised via
/// [`normalize_message_for_suppression`].
#[must_use]
pub(crate) fn looks_like_noop_status_message(normalized: &str) -> bool {
    if normalized.is_empty() {
        return false;
    }
    DUPLICATE_NOOP_PATTERN.is_match(normalized)
}

/// Prefix a dispatch message with `Task ID: <uuid>` so downstream
/// workers can parse the task linkage from the message body even when
/// the envelope loses its `task_id` metadata.
#[must_use]
pub(crate) fn format_task_dispatch_message(task_id: &str, message: &str) -> String {
    format!("Task ID: {task_id}\n\n{}", message.trim())
}

/// Trim + validate a dispatch body used by `start_task`. Go rejects
/// bodies that embed a Task ID so dispatch paths can always inject the
/// real one. Exposed for the pending `start_task` slice.
#[allow(dead_code)]
pub(crate) fn normalize_task_dispatch_body(message: &str) -> Result<String, TaskDispatchError> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err(TaskDispatchError::MessageRequired);
    }
    let existing = extract_task_id(trimmed);
    if !existing.is_empty() {
        return Err(TaskDispatchError::ContainsTaskId(existing));
    }
    Ok(trimmed.to_owned())
}

#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub(crate) enum TaskDispatchError {
    #[error("message is required")]
    MessageRequired,
    #[error(
        "message must not include Task ID {0:?}; start_task injects the new task ID automatically"
    )]
    ContainsTaskId(String),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TaskLifecycleError {
    #[error("invalid task start mode {0:?}")]
    StartMode(String),
    #[error("invalid task workflow mode {0:?}")]
    WorkflowMode(String),
    #[error("invalid task priority {0:?}")]
    Priority(String),
}

/// Build the reminder text dispatch paths send when a task needs a
/// follow-up nudge (retry / rehydrate). Mirrors Go's
/// `buildTaskReminderMessage` down to the Operator note suffix so the
/// downstream agent sees a byte-identical prompt regardless of which
/// binary produced it.
#[must_use]
pub(crate) fn build_task_reminder_message(task: &Task, note: &str) -> String {
    let title = task.title.trim();
    let description = task.description.trim();
    let status = status_label(&task.status);
    let base = if description.is_empty() {
        format!(
            "Task ID: {id}\n\nTask: {title}\nCurrent task status: {status}\nThe daemon task registry still shows this task as runnable. Call get_task for the latest structured context, then continue or report a blocker.",
            id = task.id,
            title = title,
            status = status,
        )
    } else {
        format!(
            "Task ID: {id}\n\nTask: {title}\nDescription: {description}\nCurrent task status: {status}\nThe daemon task registry still shows this task as runnable. Call get_task for the latest structured context, then continue or report a blocker.",
            id = task.id,
            title = title,
            description = description,
            status = status,
        )
    };
    if note.is_empty() {
        base
    } else {
        format!("{base}\n\nOperator note: {note}")
    }
}

/// Pick the dispatch body used when re-hydrating a runnable task.
/// Matches Go's `taskDispatchContent`: prefer the stored
/// `dispatch_message` when it's present and the operator note is
/// empty; otherwise fall back to the reminder template. Exposed
/// ahead of the runnable-rehydrate slice that will consume it.
#[must_use]
#[allow(dead_code)]
pub(crate) fn task_dispatch_content(task: &Task, note: &str) -> String {
    if note.is_empty() && !task.dispatch_message.trim().is_empty() {
        return task.dispatch_message.clone();
    }
    build_task_reminder_message(task, note)
}

/// Build a `Message` whose `task_id` field is populated from the body
/// when present. Mirrors Go's `taskAwareMessage` — the daemon stamps
/// the id + `created_at` when the message lands in the queue, so
/// callers leave those blank.
#[must_use]
pub(crate) fn task_aware_message(from: &str, to: &str, content: &str) -> Message {
    Message {
        id: String::new(),
        from: from.to_owned(),
        to: to.to_owned(),
        content: content.to_owned(),
        task_id: extract_task_id(content),
        created_at: Utc::now(),
    }
}

fn status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

/// Validate and default task lifecycle options. Mirrors Go's
/// `parseTaskLifecycleOptions` including its "empty → default" rules.
pub(crate) fn parse_task_lifecycle_options(
    start_mode: &str,
    workflow_mode: &str,
    priority: &str,
) -> Result<(TaskStartMode, TaskWorkflowMode, TaskPriority), TaskLifecycleError> {
    let start = match start_mode.trim() {
        "" | "default" => TaskStartMode::Default,
        "fresh" => TaskStartMode::Fresh,
        other => return Err(TaskLifecycleError::StartMode(other.to_owned())),
    };
    let workflow = match workflow_mode.trim() {
        "" | "parallel" => TaskWorkflowMode::Parallel,
        "serial" => TaskWorkflowMode::Serial,
        other => return Err(TaskLifecycleError::WorkflowMode(other.to_owned())),
    };
    let prio = match priority.trim() {
        "" | "normal" => TaskPriority::Normal,
        "low" => TaskPriority::Low,
        "high" => TaskPriority::High,
        "urgent" => TaskPriority::Urgent,
        other => return Err(TaskLifecycleError::Priority(other.to_owned())),
    };
    Ok((start, workflow, prio))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_task_id_pulls_uuid_from_body() {
        let body = "Task ID: 11111111-2222-3333-4444-555555555555\n\nplease do X";
        assert_eq!(
            extract_task_id(body),
            "11111111-2222-3333-4444-555555555555"
        );
    }

    #[test]
    fn extract_task_id_returns_empty_when_missing() {
        assert_eq!(extract_task_id("just a note"), "");
    }

    #[test]
    fn normalize_collapses_whitespace_and_lowercases() {
        assert_eq!(
            normalize_message_for_suppression("  HELLO\tworld  "),
            "hello world"
        );
    }

    #[test]
    fn noop_status_detection() {
        let msg = normalize_message_for_suppression("ack, on it");
        assert!(looks_like_noop_status_message(&msg));
        let msg2 = normalize_message_for_suppression("Finished refactor of module X");
        assert!(!looks_like_noop_status_message(&msg2));
    }

    #[test]
    fn format_dispatch_message_prefixes_task_id() {
        let out = format_task_dispatch_message("abc", "  please do X  ");
        assert_eq!(out, "Task ID: abc\n\nplease do X");
    }

    #[test]
    fn normalize_task_dispatch_body_rejects_embedded_task_id() {
        let err = normalize_task_dispatch_body("Task ID: 11111111-2222-3333-4444-555555555555 hi")
            .expect_err("embedded task id must fail");
        matches!(err, TaskDispatchError::ContainsTaskId(_));
    }

    #[test]
    fn parse_lifecycle_options_defaults_blanks() {
        let (start, workflow, prio) =
            parse_task_lifecycle_options("", "", "").expect("defaults must parse");
        assert_eq!(start, TaskStartMode::Default);
        assert_eq!(workflow, TaskWorkflowMode::Parallel);
        assert_eq!(prio, TaskPriority::Normal);
    }

    #[test]
    fn parse_lifecycle_options_rejects_unknown_values() {
        let err =
            parse_task_lifecycle_options("later", "", "").expect_err("unknown start must fail");
        matches!(err, TaskLifecycleError::StartMode(_));
    }
}
