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

use ax_proto::types::Task;

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

#[cfg(test)]
mod tests {
    use super::*;
    use ax_proto::types::{TaskStartMode, TaskStatus};
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
}
