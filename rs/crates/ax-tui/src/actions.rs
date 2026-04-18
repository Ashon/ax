//! Quick-action overlay — port of `cmd/watch_actions.go`. When open,
//! the overlay takes over key input and lets operators run lifecycle
//! controls against the workspace under the sidebar cursor:
//! interrupt (Escape via tmux) and restart/stop (goes through
//! `ax_workspace::{restart,stop}_named_target`). Flow-switching
//! actions (show messages / tasks) just flip the stream view.
//!
//! Restart + stop require a confirmation enter (Go does the same)
//! so stray key presses don't nuke a running agent.

use std::time::{Duration, Instant};

use ax_workspace::{restart_named_target, stop_named_target, RealTmux};

use crate::state::App;
use crate::stream::StreamView;

pub(crate) const NOTICE_TTL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuickActionId {
    ShowTasks,
    ShowMessages,
    Interrupt,
    Restart,
    Stop,
}

impl QuickActionId {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ShowTasks => "Open tasks",
            Self::ShowMessages => "Open messages",
            Self::Interrupt => "Interrupt",
            Self::Restart => "Restart",
            Self::Stop => "Stop",
        }
    }

    pub(crate) fn requires_confirmation(self) -> bool {
        matches!(self, Self::Restart | Self::Stop)
    }

    pub(crate) fn confirm_prompt(self) -> &'static str {
        match self {
            Self::Restart => "confirm restart?",
            Self::Stop => "confirm stop?",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuickAction {
    pub id: QuickActionId,
}

pub(crate) fn default_actions() -> Vec<QuickAction> {
    vec![
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
}

#[derive(Debug, Clone, Default)]
pub(crate) struct QuickActionState {
    pub open: bool,
    pub actions: Vec<QuickAction>,
    pub selected: usize,
    pub confirm: bool,
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
    Notice { text: String, error: bool },
    ChangeStream(StreamView),
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
                app.stream = view;
            }
            ActionOutcome::Close => {
                app.quick_actions.open = false;
                app.quick_actions.confirm = false;
                app.quick_actions.selected = 0;
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
        ] {
            assert!(!id.label().is_empty());
        }
    }
}
