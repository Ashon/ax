//! Quick-action overlay. When open, the overlay takes over key input
//! and lets operators run lifecycle controls against the workspace
//! under the agents-panel cursor: interrupt (Escape via tmux) and
//! restart/stop (goes through
//! `ax_workspace::{restart,stop}_named_target`). Flow-switching
//! actions (show messages / tasks) just flip the stream view.
//!
//! Restart + stop require a confirmation enter so stray key presses
//! don't nuke a running agent.

use std::time::{Duration, Instant};

use ax_proto::types::{Task, TaskStatus};
use ax_workspace::{restart_named_target, stop_named_target, RealTmux};

use crate::state::App;
use crate::stream::StreamView;

pub(crate) const NOTICE_TTL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuickActionId {
    StreamTmux,
    ShowTasks,
    ShowMessages,
    Interrupt,
    Restart,
    Stop,
    TaskWake,
    TaskInterrupt,
    TaskRetry,
    TaskCancel,
}

impl QuickActionId {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::StreamTmux => "Stream tmux",
            Self::ShowTasks => "Open tasks",
            Self::ShowMessages => "Open messages",
            Self::Interrupt => "Interrupt",
            Self::Restart => "Restart",
            Self::Stop => "Stop",
            Self::TaskWake => "Wake task",
            Self::TaskInterrupt => "Interrupt assignee",
            Self::TaskRetry => "Retry task",
            Self::TaskCancel => "Cancel task",
        }
    }

    pub(crate) fn requires_confirmation(self) -> bool {
        matches!(self, Self::Restart | Self::Stop | Self::TaskCancel)
    }

    pub(crate) fn confirm_prompt(self, target: &str) -> String {
        match self {
            Self::Restart => format!("confirm restart {target}?"),
            Self::Stop => format!("confirm stop {target}?"),
            Self::TaskCancel => format!("confirm cancel task {target}?"),
            _ => String::new(),
        }
    }

    pub(crate) fn is_task_action(self) -> bool {
        matches!(
            self,
            Self::TaskWake | Self::TaskInterrupt | Self::TaskRetry | Self::TaskCancel
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuickAction {
    pub id: QuickActionId,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn default_actions() -> Vec<QuickAction> {
    contextual_actions(true)
}

pub(crate) fn contextual_actions(has_session: bool) -> Vec<QuickAction> {
    vec![
        QuickAction {
            id: QuickActionId::StreamTmux,
        },
        QuickAction {
            id: QuickActionId::ShowTasks,
        },
        QuickAction {
            id: QuickActionId::ShowMessages,
        },
        QuickAction {
            id: QuickActionId::Interrupt,
        },
        QuickAction {
            id: QuickActionId::Restart,
        },
        QuickAction {
            id: QuickActionId::Stop,
        },
    ]
    .into_iter()
    .filter(|action| {
        has_session
            || !matches!(
                action.id,
                QuickActionId::StreamTmux | QuickActionId::Interrupt | QuickActionId::Stop
            )
    })
    .collect()
}

pub(crate) fn task_actions(task: &Task) -> Vec<QuickAction> {
    if !matches!(
        task.status,
        TaskStatus::Pending | TaskStatus::InProgress | TaskStatus::Blocked
    ) {
        return Vec::new();
    }
    [
        QuickActionId::TaskWake,
        QuickActionId::TaskInterrupt,
        QuickActionId::TaskRetry,
        QuickActionId::TaskCancel,
    ]
    .into_iter()
    .filter(|id| *id != QuickActionId::TaskInterrupt || !task.assignee.is_empty())
    .map(|id| QuickAction { id })
    .collect()
}

#[derive(Debug, Clone, Default)]
pub(crate) struct QuickActionState {
    pub open: bool,
    pub actions: Vec<QuickAction>,
    pub selected: usize,
    pub confirm: bool,
    pub target_workspace: String,
    pub target_task_id: String,
    pub target_task_version: i64,
}

impl QuickActionState {
    pub(crate) fn current(&self) -> Option<QuickAction> {
        self.actions.get(self.selected).copied()
    }
}

/// Side-effects the TUI can take after an action resolves. The app
/// loop applies them so render/input stay in one place and
/// `actions.rs` stays testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActionOutcome {
    NeedsConfirm,
    Notice {
        text: String,
        error: bool,
    },
    ChangeStream(StreamView),
    /// Enter single-agent streaming view for the given workspace —
    /// body pane becomes a full-pane live tmux capture mirror.
    StartStreaming(String),
    Close,
}

/// Run the currently-selected quick action against the target
/// workspace. Returns a list of outcomes for the caller to apply;
/// the function itself never touches the terminal.
pub(crate) fn run_selected(state: &QuickActionState, target: &str) -> Vec<ActionOutcome> {
    let Some(action) = state.current() else {
        return vec![ActionOutcome::Close];
    };
    if action.id.requires_confirmation() && !state.confirm {
        return vec![ActionOutcome::NeedsConfirm];
    }
    match action.id {
        QuickActionId::StreamTmux => {
            // Switch to the stream tab first so the caller sees the
            // live mirror the moment the overlay closes. Without the
            // view change the user would remain on whatever tab was
            // active and wonder if the action did anything.
            vec![
                ActionOutcome::ChangeStream(StreamView::Stream),
                ActionOutcome::StartStreaming(target.to_owned()),
                ActionOutcome::Close,
            ]
        }
        QuickActionId::ShowTasks => {
            vec![
                ActionOutcome::ChangeStream(StreamView::Tasks),
                ActionOutcome::Close,
            ]
        }
        QuickActionId::ShowMessages => {
            vec![
                ActionOutcome::ChangeStream(StreamView::Messages),
                ActionOutcome::Close,
            ]
        }
        QuickActionId::Interrupt => match ax_tmux::interrupt_workspace(target) {
            Ok(()) => vec![
                ActionOutcome::Notice {
                    text: format!("Interrupted {target}"),
                    error: false,
                },
                ActionOutcome::Close,
            ],
            Err(e) => vec![ActionOutcome::Notice {
                text: e.to_string(),
                error: true,
            }],
        },
        QuickActionId::Restart | QuickActionId::Stop => {
            // Restart/stop need socket/config/ax_bin context — surface
            // a "not wired" notice so the operator knows to use
            // `ax stop <name>` from the shell for now. The live path
            // runs through `apply_lifecycle` below.
            vec![ActionOutcome::Notice {
                text: format!("Lifecycle ({}) needs --config context", action.id.label()),
                error: true,
            }]
        }
        QuickActionId::TaskWake
        | QuickActionId::TaskInterrupt
        | QuickActionId::TaskRetry
        | QuickActionId::TaskCancel => vec![ActionOutcome::Close],
    }
}

/// Live lifecycle helper: used when the app has a resolved config
/// path + `ax_bin` to pass through. Kept separate from `run_selected`
/// so the decision tree stays unit-testable without IO.
pub(crate) fn apply_lifecycle(
    action: QuickActionId,
    target: &str,
    socket_path: &std::path::Path,
    config_path: &std::path::Path,
    ax_bin: &std::path::Path,
) -> Vec<ActionOutcome> {
    let result = match action {
        QuickActionId::Restart => {
            restart_named_target(&RealTmux, socket_path, config_path, ax_bin, target)
                .map(|resolved| format!("Restart requested for {}", resolved.name))
        }
        QuickActionId::Stop => {
            stop_named_target(&RealTmux, socket_path, config_path, ax_bin, target)
                .map(|resolved| format!("Stopped {}", resolved.name))
        }
        _ => return vec![ActionOutcome::Close],
    };
    match result {
        Ok(text) => vec![
            ActionOutcome::Notice { text, error: false },
            ActionOutcome::Close,
        ],
        Err(e) => vec![ActionOutcome::Notice {
            text: e.to_string(),
            error: true,
        }],
    }
}

/// Selection movement for the overlay list. Wraps clamp so the
/// cursor never jumps past the visible range.
pub(crate) fn move_overlay_selection(state: &mut QuickActionState, delta: i32) {
    if state.actions.is_empty() {
        state.selected = 0;
        return;
    }
    let n = state.actions.len() as i32;
    let next = (state.selected as i32 + delta).clamp(0, n - 1) as usize;
    state.selected = next;
    state.confirm = false;
}

#[derive(Debug, Clone)]
pub(crate) struct Notice {
    pub text: String,
    pub error: bool,
    pub expires_at: Instant,
}

impl Notice {
    pub(crate) fn new(text: String, error: bool) -> Self {
        Self {
            text,
            error,
            expires_at: Instant::now() + NOTICE_TTL,
        }
    }
}

pub(crate) fn apply_outcomes(app: &mut App, outcomes: Vec<ActionOutcome>) {
    let mut need_confirm = false;
    for outcome in outcomes {
        match outcome {
            ActionOutcome::NeedsConfirm => {
                need_confirm = true;
            }
            ActionOutcome::Notice { text, error } => {
                app.quick_notice = Some(Notice::new(text, error));
            }
            ActionOutcome::ChangeStream(view) => {
                // `streamed_workspace` used to be cleared here because
                // the stream view was a full-pane hijack. It's a
                // regular tab now, so the mirror target persists
                // across tab switches — re-open `stream` and the
                // capture resumes.
                app.stream = view;
            }
            ActionOutcome::StartStreaming(workspace) => {
                app.streamed_workspace = Some(workspace);
            }
            ActionOutcome::Close => {
                app.quick_actions.open = false;
                app.quick_actions.confirm = false;
                app.quick_actions.selected = 0;
                app.quick_actions.target_workspace.clear();
                app.quick_actions.target_task_id.clear();
                app.quick_actions.target_task_version = 0;
            }
        }
    }
    if need_confirm {
        app.quick_actions.confirm = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_state(id: QuickActionId) -> QuickActionState {
        let mut state = QuickActionState {
            open: true,
            actions: default_actions(),
            selected: 0,
            confirm: false,
            ..QuickActionState::default()
        };
        state.selected = state
            .actions
            .iter()
            .position(|a| a.id == id)
            .expect("action present");
        state
    }

    #[test]
    fn run_selected_for_stream_switches_view_and_closes() {
        let state = open_state(QuickActionId::ShowMessages);
        let outcomes = run_selected(&state, "alpha");
        assert_eq!(
            outcomes,
            vec![
                ActionOutcome::ChangeStream(StreamView::Messages),
                ActionOutcome::Close
            ]
        );
    }

    #[test]
    fn run_selected_for_stream_flips_tab_and_sets_mirror_target() {
        let state = open_state(QuickActionId::StreamTmux);
        let outcomes = run_selected(&state, "alpha");
        assert_eq!(
            outcomes,
            vec![
                ActionOutcome::ChangeStream(StreamView::Stream),
                ActionOutcome::StartStreaming("alpha".into()),
                ActionOutcome::Close,
            ],
            "stream action flips to the Stream tab first so the live \
             mirror appears immediately when the overlay closes",
        );
    }

    #[test]
    fn change_stream_preserves_mirror_target_so_users_can_tab_away() {
        let mut app = crate::state::App::new();
        app.streamed_workspace = Some("alpha".into());
        app.stream = StreamView::Stream;
        apply_outcomes(
            &mut app,
            vec![ActionOutcome::ChangeStream(StreamView::Tasks)],
        );
        assert_eq!(app.stream, StreamView::Tasks);
        assert_eq!(
            app.streamed_workspace.as_deref(),
            Some("alpha"),
            "tab switch must not clear the stream target — otherwise \
             returning to the Stream tab loses the active mirror",
        );
    }

    #[test]
    fn run_selected_requires_confirmation_for_stop_and_restart() {
        let state = open_state(QuickActionId::Stop);
        let outcomes = run_selected(&state, "alpha");
        assert_eq!(outcomes, vec![ActionOutcome::NeedsConfirm]);

        let mut confirmed = state.clone();
        confirmed.confirm = true;
        let outcomes = run_selected(&confirmed, "alpha");
        // Stop requires the caller to dispatch lifecycle — result is the
        // "needs context" notice unless apply_lifecycle runs.
        assert!(matches!(
            outcomes[0],
            ActionOutcome::Notice { error: true, .. }
        ));
    }

    #[test]
    fn move_overlay_selection_clamps_to_bounds() {
        let mut state = open_state(QuickActionId::ShowTasks);
        move_overlay_selection(&mut state, 100);
        assert_eq!(state.selected, state.actions.len() - 1);
        move_overlay_selection(&mut state, -100);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn labels_cover_every_variant() {
        for id in [
            QuickActionId::ShowTasks,
            QuickActionId::ShowMessages,
            QuickActionId::Interrupt,
            QuickActionId::Restart,
            QuickActionId::Stop,
            QuickActionId::TaskWake,
            QuickActionId::TaskInterrupt,
            QuickActionId::TaskRetry,
            QuickActionId::TaskCancel,
        ] {
            assert!(!id.label().is_empty());
        }
    }

    #[test]
    fn contextual_actions_hide_session_only_actions_without_session() {
        let ids: Vec<_> = contextual_actions(false)
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert!(!ids.contains(&QuickActionId::StreamTmux));
        assert!(!ids.contains(&QuickActionId::Interrupt));
        assert!(!ids.contains(&QuickActionId::Stop));
        assert!(ids.contains(&QuickActionId::Restart));
        assert!(ids.contains(&QuickActionId::ShowTasks));
    }

    #[test]
    fn task_actions_only_include_active_task_remediation() {
        let mut task = mock_task(TaskStatus::InProgress);
        let ids: Vec<_> = task_actions(&task).into_iter().map(|a| a.id).collect();
        assert!(ids.contains(&QuickActionId::TaskWake));
        assert!(ids.contains(&QuickActionId::TaskInterrupt));
        assert!(ids.contains(&QuickActionId::TaskRetry));
        assert!(ids.contains(&QuickActionId::TaskCancel));

        task.status = TaskStatus::Completed;
        assert!(task_actions(&task).is_empty());
    }

    #[test]
    fn confirm_prompt_names_target_workspace() {
        assert_eq!(
            QuickActionId::Restart.confirm_prompt("alpha"),
            "confirm restart alpha?"
        );
        assert_eq!(
            QuickActionId::Stop.confirm_prompt("alpha"),
            "confirm stop alpha?"
        );
    }

    fn mock_task(status: TaskStatus) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: "task123".into(),
            title: "task".into(),
            description: String::new(),
            assignee: "alpha".into(),
            created_by: "orch".into(),
            parent_task_id: String::new(),
            child_task_ids: Vec::new(),
            version: 1,
            status,
            start_mode: ax_proto::types::TaskStartMode::Default,
            workflow_mode: None,
            priority: None,
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
            created_at: now,
            updated_at: now,
        }
    }
}
