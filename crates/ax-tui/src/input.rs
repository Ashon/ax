//! Key event → app-state transitions. Kept separate so the
//! dispatch logic can be unit-tested without a real terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::actions::{
    apply_outcomes, default_actions, move_overlay_selection, run_selected, ActionOutcome,
    QuickActionId,
};
use crate::state::{App, Focus, PendingLifecycle};
use crate::stream::StreamView;

pub(crate) fn handle_key(app: &mut App, event: KeyEvent) {
    if event.kind == KeyEventKind::Release {
        return;
    }
    // Global exits take precedence over overlay state so q / ctrl-c
    // always closes the TUI, even mid-confirmation.
    match event.code {
        KeyCode::Char('q') => {
            app.quit = true;
            return;
        }
        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            app.quit = true;
            return;
        }
        _ => {}
    }

    if app.help_open {
        // Help is a pure reference surface — `?` / Esc close it; q
        // still quits (handled above). Everything else is swallowed
        // so a stray arrow doesn't scroll the panel behind the
        // overlay while the user reads the cheatsheet.
        if matches!(event.code, KeyCode::Char('?') | KeyCode::Esc) {
            app.help_open = false;
        }
        return;
    }

    if app.quick_actions.open {
        handle_overlay_key(app, event);
        return;
    }

    // `?` toggles the help overlay from any non-overlay context. It's
    // global so operators can reach it without first parking focus on
    // a specific panel.
    if matches!(event.code, KeyCode::Char('?')) {
        app.help_open = true;
        return;
    }

    // Global bindings — active regardless of which panel is focused.
    // Arrow keys are deliberately *not* global: each panel owns them
    // so a stray ↑/↓ can't leak across scopes. Tab switching moves to
    // its own dedicated keys (Tab/Shift-Tab + 1-4) so operators can
    // flip views without losing their place in Agents.
    match event.code {
        KeyCode::Char('[') | KeyCode::Char(']') => {
            app.cycle_focus(1);
            return;
        }
        KeyCode::Tab => {
            app.step_stream(1);
            return;
        }
        KeyCode::BackTab => {
            app.step_stream(-1);
            return;
        }
        KeyCode::Char(c @ ('1' | '2' | '3' | '4' | '5')) => {
            let idx = (c as u8 - b'1') as usize;
            app.select_stream(idx);
            // Focus intentionally preserved — peeking at a tab from
            // the list shouldn't strand the cursor in the detail
            // pane or vice versa.
            return;
        }
        KeyCode::Char('f') => {
            app.cycle_task_filter();
            return;
        }
        _ => {}
    }

    // Focus-scoped dispatch. `List` routes the keys to the active
    // tab's list handler; `Detail` drives the shared detail scroll
    // state. Either focus returns no-op for irrelevant keys.
    match app.focus {
        Focus::List => handle_list_key(app, event),
        Focus::Detail => handle_detail_key(app, event),
    }
}

fn handle_list_key(app: &mut App, event: KeyEvent) {
    // Esc from the list cycles Back-a-step inside the list scope —
    // clears any lingering notice but doesn't steal focus. `[/]`
    // is the move to the detail pane.
    match app.stream {
        StreamView::Agents => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Enter => open_overlay(app),
            _ => {}
        },
        StreamView::Tasks => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.move_task_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_task_selection(1),
            _ => {}
        },
        // Messages scroll is tail-anchored: ↑ walks back into history,
        // ↓ walks back toward the latest. `G`/End returns to follow
        // mode; `g`/Home jumps to the oldest entry.
        StreamView::Messages => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.scroll_messages(1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_messages(-1),
            KeyCode::PageUp => app.scroll_messages(10),
            KeyCode::PageDown => app.scroll_messages(-10),
            KeyCode::Home | KeyCode::Char('g') => app.messages_to_head(),
            KeyCode::End | KeyCode::Char('G') => app.messages_to_tail(),
            _ => {}
        },
        // Tokens is a top-anchored sorted list.
        StreamView::Tokens => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.scroll_tokens(-1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_tokens(1),
            KeyCode::PageUp => app.scroll_tokens(-10),
            KeyCode::PageDown => app.scroll_tokens(10),
            KeyCode::Home | KeyCode::Char('g') => app.tokens_to_head(),
            KeyCode::End | KeyCode::Char('G') => app.tokens_to_tail(),
            _ => {}
        },
        // Stream's "list" is the live capture tail — no manual scroll
        // surface yet; follow-up slice adds it alongside a scroll
        // cursor on the capture buffer.
        StreamView::Stream => {}
    }
}

fn handle_detail_key(app: &mut App, event: KeyEvent) {
    match event.code {
        KeyCode::Up | KeyCode::Char('k') => app.detail_scroll.shift(-1),
        KeyCode::Down | KeyCode::Char('j') => app.detail_scroll.shift(1),
        KeyCode::PageUp => app.detail_scroll.shift(-10),
        KeyCode::PageDown => app.detail_scroll.shift(10),
        KeyCode::Home | KeyCode::Char('g') => app.detail_scroll.reset(),
        // Esc drops focus back to the list so operators can exit the
        // detail scope without hitting `[/]` explicitly.
        KeyCode::Esc => app.focus = Focus::List,
        _ => {}
    }
}

/// Route a mouse wheel event to the focused panel's scroll handler.
/// `direction` is `-1` for wheel-up (scroll toward top / into history)
/// and `+1` for wheel-down. Kept focus-driven for now — hover-based
/// routing would need the last-rendered pane rects plumbed in.
///
/// Mapping mirrors the keyboard: Agents moves the selection cursor,
/// Tasks the task cursor, Messages walks history (wheel-up = older),
/// Tokens pans the sorted list.
pub(crate) fn handle_scroll(app: &mut App, direction: i32) {
    if app.quick_actions.open || app.help_open {
        // Overlays swallow the wheel so a stray scroll doesn't flicker
        // the panel underneath while a destructive confirm or the
        // help cheatsheet is visible.
        return;
    }
    match app.focus {
        Focus::List => match app.stream {
            StreamView::Agents => app.move_selection(direction),
            StreamView::Tasks => app.move_task_selection(direction),
            StreamView::Messages => app.scroll_messages(-direction),
            StreamView::Tokens => app.scroll_tokens(direction),
            // Stream is a live tail — no manual scroll surface yet.
            StreamView::Stream => {}
        },
        Focus::Detail => app.detail_scroll.shift(direction),
    }
}

fn open_overlay(app: &mut App) {
    if app.selected_workspace().is_none() {
        return;
    }
    app.quick_actions.actions = default_actions();
    app.quick_actions.selected = 0;
    app.quick_actions.confirm = false;
    app.quick_actions.open = true;
}

fn handle_overlay_key(app: &mut App, event: KeyEvent) {
    match event.code {
        KeyCode::Esc => {
            app.quick_actions.open = false;
            app.quick_actions.confirm = false;
            app.quick_actions.selected = 0;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            move_overlay_selection(&mut app.quick_actions, -1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            move_overlay_selection(&mut app.quick_actions, 1);
        }
        KeyCode::Enter => activate_overlay(app),
        _ => {}
    }
}

fn activate_overlay(app: &mut App) {
    let Some(target) = app.selected_workspace().map(str::to_owned) else {
        app.quick_actions.open = false;
        return;
    };
    let Some(action) = app.quick_actions.current() else {
        return;
    };
    // Lifecycle actions need paths we don't have in state.rs, so the
    // input handler queues them for the app loop.
    if matches!(action.id, QuickActionId::Restart | QuickActionId::Stop) {
        if !app.quick_actions.confirm {
            let outcomes = vec![ActionOutcome::NeedsConfirm];
            apply_outcomes(app, outcomes);
            return;
        }
        app.pending_lifecycle = Some(PendingLifecycle {
            action: action.id,
            workspace: target,
        });
        // Close the overlay while the app loop executes; the notice
        // emitted by `apply_lifecycle` will surface the result.
        app.quick_actions.open = false;
        app.quick_actions.confirm = false;
        return;
    }
    let outcomes = run_selected(&app.quick_actions, &target);
    apply_outcomes(app, outcomes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::QuickActionId;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_opens_overlay_when_workspace_selected() {
        let mut app = App::new();
        // No agents panel = no workspace → enter is a no-op.
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(!app.quick_actions.open);

        app.agent_entries = vec![crate::agents::AgentEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: Some(0),
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        app.selected_entry = 0;
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(app.quick_actions.open);
        assert!(!app.quick_actions.actions.is_empty());
    }

    #[test]
    fn overlay_enter_on_restart_sets_needs_confirm_then_queues_lifecycle() {
        let mut app = App::new();
        app.agent_entries = vec![crate::agents::AgentEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: Some(0),
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        app.selected_entry = 0;
        handle_key(&mut app, press(KeyCode::Enter));
        // jump selection to Restart.
        let restart_idx = app
            .quick_actions
            .actions
            .iter()
            .position(|a| a.id == QuickActionId::Restart)
            .unwrap();
        app.quick_actions.selected = restart_idx;

        handle_key(&mut app, press(KeyCode::Enter));
        assert!(app.quick_actions.confirm);
        assert!(app.pending_lifecycle.is_none());

        handle_key(&mut app, press(KeyCode::Enter));
        let pending = app.pending_lifecycle.clone().expect("queued");
        assert_eq!(pending.action, QuickActionId::Restart);
        assert_eq!(pending.workspace, "alpha");
        // Overlay closed so next paint shows the regular footer.
        assert!(!app.quick_actions.open);
    }

    #[test]
    fn q_sets_quit() {
        let mut app = App::new();
        handle_key(&mut app, press(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[test]
    fn ctrl_c_sets_quit() {
        let mut app = App::new();
        let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        handle_key(&mut app, event);
        assert!(app.quit);
    }

    #[test]
    fn brackets_toggle_focus_between_panels() {
        let mut app = App::new();
        assert_eq!(app.focus, Focus::List);
        handle_key(&mut app, press(KeyCode::Char(']')));
        assert_eq!(app.focus, Focus::Detail);
        handle_key(&mut app, press(KeyCode::Char(']')));
        assert_eq!(app.focus, Focus::List);
        // `[` and `]` behave identically with only two panels.
        handle_key(&mut app, press(KeyCode::Char('[')));
        assert_eq!(app.focus, Focus::Detail);
    }

    #[test]
    fn tab_key_cycles_tabs_from_any_focus() {
        let mut app = App::new();
        // Default is Agents; Tab walks to Messages without stealing
        // the List↔Detail focus so operators can peek at other tabs.
        assert_eq!(app.stream, crate::stream::StreamView::Agents);
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.stream, crate::stream::StreamView::Messages);
        assert_eq!(app.focus, Focus::List);

        app.focus = Focus::Detail;
        handle_key(&mut app, press(KeyCode::BackTab));
        assert_eq!(app.stream, crate::stream::StreamView::Agents);
        assert_eq!(app.focus, Focus::Detail);
    }

    #[test]
    fn arrow_keys_stay_inside_focused_panel() {
        let mut app = App::new();
        app.agent_entries = vec![
            crate::agents::AgentEntry {
                label: "alpha".into(),
                workspace: "alpha".into(),
                session_index: Some(0),
                level: 0,
                group: false,
                reconcile: String::new(),
            },
            crate::agents::AgentEntry {
                label: "beta".into(),
                workspace: "beta".into(),
                session_index: Some(1),
                level: 0,
                group: false,
                reconcile: String::new(),
            },
        ];
        app.selected_entry = 0;

        // Agents focus: Down moves the cursor.
        handle_key(&mut app, press(KeyCode::Down));
        assert_eq!(app.selected_entry, 1);

        // Body focus: Down/Up must not leak back into Agents.
        app.focus = Focus::Detail;
        let before = app.selected_entry;
        handle_key(&mut app, press(KeyCode::Up));
        assert_eq!(app.focus, Focus::Detail, "Up stays inside Body");
        assert_eq!(app.selected_entry, before, "Body arrows don't touch Agents");

        // Body Left/Right no longer cycle tabs — they're no-ops so the
        // tab strip isn't an accidental target.
        let stream_before = app.stream;
        handle_key(&mut app, press(KeyCode::Right));
        assert_eq!(app.stream, stream_before);
    }

    #[test]
    fn digit_keys_jump_tab_without_changing_focus() {
        let mut app = App::new();
        assert_eq!(app.focus, Focus::List);
        // `4` is now Tokens (Agents=1, Messages=2, Tasks=3, Tokens=4,
        // Stream=5); focus must stay on List so digits are a pure
        // view peek.
        handle_key(&mut app, press(KeyCode::Char('4')));
        assert_eq!(app.stream, crate::stream::StreamView::Tokens);
        assert_eq!(
            app.focus,
            Focus::List,
            "digit keys preserve current focus"
        );
        // `5` jumps to Stream, exercising the 5th slot.
        handle_key(&mut app, press(KeyCode::Char('5')));
        assert_eq!(app.stream, crate::stream::StreamView::Stream);
    }

    #[test]
    fn esc_returns_from_body_to_agents() {
        let mut app = App::new();
        app.focus = Focus::Detail;
        handle_key(&mut app, press(KeyCode::Esc));
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn mouse_wheel_routes_by_focus_and_tab() {
        let mut app = App::new();
        app.agent_entries = vec![
            crate::agents::AgentEntry {
                label: "alpha".into(),
                workspace: "alpha".into(),
                session_index: Some(0),
                level: 0,
                group: false,
                reconcile: String::new(),
            },
            crate::agents::AgentEntry {
                label: "beta".into(),
                workspace: "beta".into(),
                session_index: Some(1),
                level: 0,
                group: false,
                reconcile: String::new(),
            },
        ];
        app.selected_entry = 0;
        assert_eq!(app.focus, Focus::List);
        assert_eq!(app.stream, StreamView::Agents);

        // List + Agents: wheel-down advances the agent cursor.
        handle_scroll(&mut app, 1);
        assert_eq!(app.selected_entry, 1);
        handle_scroll(&mut app, -1);
        assert_eq!(app.selected_entry, 0);

        // List + Messages: wheel-up walks back into history (direction
        // inverts because message scroll is entries-from-tail).
        app.stream = StreamView::Messages;
        handle_scroll(&mut app, -1);
        assert_eq!(app.messages_cursor.index, 1);
        handle_scroll(&mut app, 1);
        assert_eq!(app.messages_cursor.index, 0);

        // List + Tokens: wheel-down pans toward the last row.
        app.stream = StreamView::Tokens;
        handle_scroll(&mut app, 1);
        assert_eq!(app.tokens_cursor.index, 1);

        // Detail focus routes the wheel to `detail_scroll` regardless
        // of which tab is active — a single shared cursor for every
        // detail pane.
        app.focus = Focus::Detail;
        app.stream = StreamView::Agents;
        handle_scroll(&mut app, 1);
        assert_eq!(app.detail_scroll.index, 1);
        handle_scroll(&mut app, 1);
        assert_eq!(app.detail_scroll.index, 2);
    }

    #[test]
    fn question_mark_toggles_help_overlay() {
        let mut app = App::new();
        assert!(!app.help_open);
        handle_key(&mut app, press(KeyCode::Char('?')));
        assert!(app.help_open);
        // Arrow keys are swallowed while help is open so the panel
        // behind the overlay doesn't drift.
        app.agent_entries = vec![crate::agents::AgentEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: Some(0),
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        app.selected_entry = 0;
        handle_key(&mut app, press(KeyCode::Down));
        assert_eq!(app.selected_entry, 0, "arrows swallowed under help");
        handle_key(&mut app, press(KeyCode::Char('?')));
        assert!(!app.help_open);
        handle_key(&mut app, press(KeyCode::Char('?')));
        assert!(app.help_open);
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(!app.help_open, "esc also closes help");
    }

    #[test]
    fn mouse_wheel_is_ignored_while_overlay_is_open() {
        let mut app = App::new();
        app.agent_entries = vec![crate::agents::AgentEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: Some(0),
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        app.selected_entry = 0;
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(app.quick_actions.open);
        let before = app.quick_actions.selected;
        handle_scroll(&mut app, 1);
        assert_eq!(app.quick_actions.selected, before, "overlay swallows wheel");
    }
}
