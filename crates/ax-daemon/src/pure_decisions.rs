//! Pure decision functions for task-lifecycle handlers.
//!
//! The envelope handlers in `handlers.rs` mix three concerns:
//! payload decoding, state-transition decisions, and side effects
//! (queue enqueue, wake scheduler, session manager, tmux). This
//! module isolates the middle concern so it can be unit-tested
//! without spinning up a daemon or faking tmux.
//!
//! The shape is deliberately small: each function takes the
//! already-decoded state plus any read-only probes the caller can
//! provide cheaply (like `session_exists`), and returns an enum that
//! names the action the handler must carry out. Handlers stay
//! responsible for actually performing the I/O; they just stop
//! making policy decisions inline.
//!
//! Mirrors the team-core style in fleet-shell, where
//! `decideTaskRun` / `decideTeamFinalization` / `decideHookFollowUp`
//! keep the routing logic testable in isolation.

use ax_proto::types::{Task, TaskStatus};

/// Sub-action the `intervene_task` handler should run. The string
/// on the wire is user-supplied, so an unknown action surfaces as
/// [`InterventionPlan::Invalid`] rather than a silent fallthrough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterventionPlan {
    Wake,
    Interrupt,
    Retry,
    Invalid(String),
}

/// Parse the wire-level `action` string into a structured plan.
/// Trims whitespace and normalises case so minor transport-level
/// variance doesn't bleed into handler behaviour.
pub(crate) fn plan_intervention(action: &str) -> InterventionPlan {
    match action.trim().to_ascii_lowercase().as_str() {
        "wake" => InterventionPlan::Wake,
        "interrupt" => InterventionPlan::Interrupt,
        "retry" => InterventionPlan::Retry,
        _ => InterventionPlan::Invalid(action.to_owned()),
    }
}

/// How the handler should deliver a wake to the task's assignee.
/// The three outcomes mirror the three branches `dispatch_task_wake`
/// used to carry inline: go through the session manager when we
/// know the dispatch config, fall back to a direct tmux wake when
/// the session is already up, and surface an error when we have
/// neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WakePlan {
    EnsureRunnable {
        config_path: String,
        assignee: String,
    },
    DirectWake {
        assignee: String,
    },
    SessionMissing {
        assignee: String,
    },
}

/// Decide how to wake `task.assignee`. The `session_exists` probe
/// is injected so unit tests can drive the "session alive" branch
/// without tmux.
pub(crate) fn plan_task_wake<F>(task: &Task, session_exists: F) -> WakePlan
where
    F: Fn(&str) -> bool,
{
    let config_path = task.dispatch_config_path.trim();
    if !config_path.is_empty() {
        return WakePlan::EnsureRunnable {
            config_path: config_path.to_owned(),
            assignee: task.assignee.clone(),
        };
    }
    if session_exists(&task.assignee) {
        return WakePlan::DirectWake {
            assignee: task.assignee.clone(),
        };
    }
    WakePlan::SessionMissing {
        assignee: task.assignee.clone(),
    }
}

/// Outcome of validating + classifying a `send_message` payload. The
/// three success branches describe how far the handler needs to go in
/// dispatch terms — enqueue-only, enqueue plus tmux `send_keys` wake,
/// or enqueue plus `ensure_runnable` (config-path driven restart).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SendMessagePlan {
    /// Transport-level rejection (e.g. self-addressed message). The
    /// handler should surface this as a logic error.
    Reject { reason: String },
    /// Normal enqueue + wake schedule. No need to (re)start sessions.
    Enqueue,
    /// Same as [`Self::Enqueue`] but the caller also passed a config
    /// path, so the dispatch backend should ensure the recipient's
    /// tmux session is alive.
    EnqueueAndEnsureRunnable { config_path: String },
}

/// Classify an incoming `send_message`. The payload fields are passed
/// raw because the handler already decoded them; this function keeps
/// the "can we even send it" and "do we need to wake the session"
/// branches in one place instead of letting them bleed into the
/// dispatch code path.
pub(crate) fn plan_send_message(sender: &str, to: &str, config_path: &str) -> SendMessagePlan {
    if to.trim().is_empty() {
        return SendMessagePlan::Reject {
            reason: "missing recipient".into(),
        };
    }
    if to == sender {
        return SendMessagePlan::Reject {
            reason: "cannot send message to self".into(),
        };
    }
    let config_path = config_path.trim();
    if config_path.is_empty() {
        SendMessagePlan::Enqueue
    } else {
        SendMessagePlan::EnqueueAndEnsureRunnable {
            config_path: config_path.to_owned(),
        }
    }
}

/// Outcome of preparing a task's initial dispatch. Mirrors
/// fleet-shell's `decideTaskRun`: a `pending` task with no payload is
/// parked as `WaitingForInput`; anything else is queued for the
/// assignee, optionally with a config-driven `ensure_runnable` so the
/// session comes back up if it had exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaskStartDispatchPlan {
    /// Task created but no dispatch message yet. The handler should
    /// return `waiting_for_input` and leave the task parked; no queue
    /// mutation, no wake.
    WaitingForInput,
    /// Queue the dispatch message for the assignee. If
    /// `config_path` is `Some`, the handler must also call
    /// `ensure_runnable` so the session exists before delivery.
    Queue {
        assignee: String,
        config_path: Option<String>,
    },
    /// Task is not eligible for a dispatch (already terminal, etc.).
    /// Surfaces as a logic error on the handler side.
    Skip { reason: String },
}

/// What the handler should do after a task-state mutation settles.
/// Right now the only signal is "task landed in a terminal state," so
/// the assignee's queue and wake schedule must be cleaned up. This
/// mirrors fleet-shell's `decideHookFollowUp`, where a state event is
/// converted to a list of actions the caller should execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaskStateFollowupPlan {
    /// Non-terminal (still live) transition. No queue/wake work needed.
    None,
    /// Task reached Completed/Failed/Cancelled/Blocked and is no longer
    /// actionable for the assignee. The handler must purge pending
    /// messages for this task and cancel the assignee's wake schedule
    /// if the queue ends up empty.
    CleanupTerminal { assignee: String, task_id: String },
}

/// Decide whether a post-mutation cleanup is required. Pure function,
/// so three different handlers (`update_task`, `cancel_task`,
/// `remove_task`) can share the same follow-up logic without each one
/// re-implementing the "is this terminal?" branch.
pub(crate) fn plan_task_state_followup(task: &Task) -> TaskStateFollowupPlan {
    match &task.status {
        TaskStatus::Completed
        | TaskStatus::Failed
        | TaskStatus::Cancelled
        | TaskStatus::Blocked => TaskStateFollowupPlan::CleanupTerminal {
            assignee: task.assignee.clone(),
            task_id: task.id.clone(),
        },
        TaskStatus::Pending | TaskStatus::InProgress => TaskStateFollowupPlan::None,
    }
}

/// Decide how `start_task` should carry out its initial dispatch. The
/// task itself is already persisted; this only picks the dispatch
/// branch based on its shape (empty body → wait, non-empty → queue,
/// optionally ensure a config-driven session).
pub(crate) fn plan_task_start_dispatch(task: &Task) -> TaskStartDispatchPlan {
    match &task.status {
        TaskStatus::Pending | TaskStatus::InProgress => {}
        other => {
            return TaskStartDispatchPlan::Skip {
                reason: format!("task already in terminal state {other:?}"),
            };
        }
    }
    if task.dispatch_message.trim().is_empty() {
        return TaskStartDispatchPlan::WaitingForInput;
    }
    let config_path = task.dispatch_config_path.trim();
    TaskStartDispatchPlan::Queue {
        assignee: task.assignee.clone(),
        config_path: (!config_path.is_empty()).then(|| config_path.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ax_proto::types::TaskStartMode;
    use chrono::Utc;

    fn task_with(assignee: &str, config_path: &str) -> Task {
        Task {
            id: "t-1".into(),
            title: "t".into(),
            description: String::new(),
            assignee: assignee.into(),
            created_by: "orch".into(),
            parent_task_id: String::new(),
            child_task_ids: Vec::new(),
            version: 1,
            status: TaskStatus::InProgress,
            start_mode: TaskStartMode::Default,
            workflow_mode: None,
            priority: None,
            stale_after_seconds: 0,
            dispatch_message: String::new(),
            dispatch_config_path: config_path.into(),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn intervention_parses_known_actions() {
        assert_eq!(plan_intervention("wake"), InterventionPlan::Wake);
        assert_eq!(plan_intervention("interrupt"), InterventionPlan::Interrupt);
        assert_eq!(plan_intervention("retry"), InterventionPlan::Retry);
    }

    #[test]
    fn intervention_normalises_case_and_whitespace() {
        assert_eq!(plan_intervention("  WAKE  "), InterventionPlan::Wake);
        assert_eq!(plan_intervention("Retry"), InterventionPlan::Retry);
    }

    #[test]
    fn intervention_rejects_unknown_action() {
        match plan_intervention("nuke") {
            InterventionPlan::Invalid(s) => assert_eq!(s, "nuke"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn wake_plan_uses_config_path_when_present() {
        let task = task_with("worker", "/path/to/config.yaml");
        // session_exists is irrelevant when the config path is set —
        // the session manager owns the "recreate missing session"
        // branch downstream.
        let plan = plan_task_wake(&task, |_| false);
        assert_eq!(
            plan,
            WakePlan::EnsureRunnable {
                config_path: "/path/to/config.yaml".into(),
                assignee: "worker".into()
            }
        );
    }

    #[test]
    fn wake_plan_falls_back_to_direct_wake_when_session_alive() {
        let task = task_with("worker", "");
        let plan = plan_task_wake(&task, |ws| ws == "worker");
        assert_eq!(
            plan,
            WakePlan::DirectWake {
                assignee: "worker".into()
            }
        );
    }

    #[test]
    fn wake_plan_errors_when_session_missing_and_no_config() {
        let task = task_with("worker", "");
        let plan = plan_task_wake(&task, |_| false);
        assert_eq!(
            plan,
            WakePlan::SessionMissing {
                assignee: "worker".into()
            }
        );
    }

    #[test]
    fn wake_plan_treats_whitespace_config_path_as_empty() {
        let task = task_with("worker", "   ");
        let plan = plan_task_wake(&task, |_| true);
        assert_eq!(
            plan,
            WakePlan::DirectWake {
                assignee: "worker".into()
            }
        );
    }

    // ---------- send_message ----------

    #[test]
    fn send_message_rejects_empty_recipient() {
        match plan_send_message("alice", "   ", "") {
            SendMessagePlan::Reject { reason } => assert!(reason.contains("missing recipient")),
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn send_message_rejects_self_addressed() {
        match plan_send_message("alice", "alice", "") {
            SendMessagePlan::Reject { reason } => assert!(reason.contains("cannot send")),
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn send_message_enqueue_without_config_path() {
        assert_eq!(
            plan_send_message("alice", "bob", ""),
            SendMessagePlan::Enqueue
        );
        // whitespace-only counts as absent
        assert_eq!(
            plan_send_message("alice", "bob", "   "),
            SendMessagePlan::Enqueue
        );
    }

    #[test]
    fn send_message_ensures_runnable_when_config_path_present() {
        assert_eq!(
            plan_send_message("alice", "bob", "/etc/team.yaml"),
            SendMessagePlan::EnqueueAndEnsureRunnable {
                config_path: "/etc/team.yaml".into(),
            }
        );
    }

    // ---------- task_start_dispatch ----------

    fn task_start_fixture(status: TaskStatus, body: &str, config_path: &str) -> Task {
        let mut task = task_with("worker", config_path);
        task.status = status;
        task.dispatch_message = body.into();
        task
    }

    #[test]
    fn start_dispatch_waits_when_body_empty() {
        let task = task_start_fixture(TaskStatus::Pending, "", "");
        assert_eq!(
            plan_task_start_dispatch(&task),
            TaskStartDispatchPlan::WaitingForInput
        );
    }

    #[test]
    fn start_dispatch_queues_without_ensure_runnable_when_no_config() {
        let task = task_start_fixture(TaskStatus::Pending, "please implement X", "");
        assert_eq!(
            plan_task_start_dispatch(&task),
            TaskStartDispatchPlan::Queue {
                assignee: "worker".into(),
                config_path: None,
            }
        );
    }

    #[test]
    fn start_dispatch_requests_ensure_runnable_with_config_path() {
        let task = task_start_fixture(TaskStatus::Pending, "please implement X", "/etc/ax.yaml");
        assert_eq!(
            plan_task_start_dispatch(&task),
            TaskStartDispatchPlan::Queue {
                assignee: "worker".into(),
                config_path: Some("/etc/ax.yaml".into()),
            }
        );
    }

    #[test]
    fn start_dispatch_skips_terminal_task() {
        let task = task_start_fixture(TaskStatus::Completed, "irrelevant", "");
        match plan_task_start_dispatch(&task) {
            TaskStartDispatchPlan::Skip { reason } => assert!(reason.contains("Completed")),
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    // ---------- task_state_followup ----------

    #[test]
    fn state_followup_is_noop_for_live_statuses() {
        for status in [TaskStatus::Pending, TaskStatus::InProgress] {
            let mut task = task_with("worker", "");
            task.id = "task-1".into();
            task.status = status;
            assert_eq!(plan_task_state_followup(&task), TaskStateFollowupPlan::None);
        }
    }

    #[test]
    fn state_followup_requests_cleanup_for_terminal_statuses() {
        for status in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
            TaskStatus::Blocked,
        ] {
            let mut task = task_with("worker", "");
            task.id = "task-1".into();
            task.status = status.clone();
            assert_eq!(
                plan_task_state_followup(&task),
                TaskStateFollowupPlan::CleanupTerminal {
                    assignee: "worker".into(),
                    task_id: "task-1".into(),
                },
                "expected cleanup plan for {status:?}",
            );
        }
    }
}
