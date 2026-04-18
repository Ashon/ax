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

    if app.quick_actions.open {
        handle_overlay_key(app, event);
        return;
    }

    // In streaming mode, `esc` exits back to the grid/stream layout
    // first. All other keys still steer the agents panel + stream
    // view so you can e.g. swap to the tasks table while a capture is
    // open.
    if app.streamed_workspace.is_some() && matches!(event.code, KeyCode::Esc) {
        app.streamed_workspace = None;
        return;
    }

    // Global bindings — active regardless of the focused panel.
    // `[` and `]` cycle panels. Tab/BackTab are deliberately *not*
    // global so the tab strip can claim them when focused.
    match event.code {
        KeyCode::Char('[') => {
            app.cycle_focus(-1);
            return;
        }
        KeyCode::Char(']') => {
            app.cycle_focus(1);
            return;
        }
        KeyCode::Char(c @ ('1' | '2' | '3')) => {
            let idx = (c as u8 - b'1') as usize;
            app.select_stream(idx);
            // Jumping to a specific tab implies the operator wants
            // to interact with its body next; move focus so they
            // don't have to chase it with `]`.
            app.focus = Focus::Body;
            return;
        }
        KeyCode::Char('f') => {
            app.cycle_task_filter();
            return;
        }
        _ => {}
    }

    // Panel-scoped dispatch.
    match app.focus {
        Focus::Agents => handle_agents_key(app, event),
        Focus::Tabs => handle_tabs_key(app, event),
        Focus::Body => handle_body_key(app, event),
    }
}

fn handle_agents_key(app: &mut App, event: KeyEvent) {
    match event.code {
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Enter => open_overlay(app),
        _ => {}
    }
}

fn handle_tabs_key(app: &mut App, event: KeyEvent) {
    match event.code {
        // Tab/Shift-Tab cycle tabs while the strip is focused — the
        // muscle-memory move for a tab row.
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => app.step_stream(1),
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => app.step_stream(-1),
        // Pressing enter on a tab "drops in" — shift focus to the
        // panel body so follow-up arrow keys navigate its contents.
        KeyCode::Enter => app.focus = Focus::Body,
        KeyCode::Up | KeyCode::Char('k') => app.focus = Focus::Agents,
        KeyCode::Down | KeyCode::Char('j') => app.focus = Focus::Body,
        _ => {}
    }
}

fn handle_body_key(app: &mut App, event: KeyEvent) {
    match app.stream {
        StreamView::Tasks => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.move_task_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_task_selection(1),
            KeyCode::Left | KeyCode::Char('h') => app.step_stream(-1),
            KeyCode::Right | KeyCode::Char('l') => app.step_stream(1),
            KeyCode::Esc => app.focus = Focus::Tabs,
            _ => {}
        },
        // Messages + Tokens are read-only today. Arrow keys still
        // offer a path back to the tab strip so keyboard-only users
        // aren't trapped.
        _ => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.focus = Focus::Tabs,
            KeyCode::Left | KeyCode::Char('h') => app.step_stream(-1),
            KeyCode::Right | KeyCode::Char('l') => app.step_stream(1),
            KeyCode::Esc => app.focus = Focus::Tabs,
            _ => {}
        },
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
    fn brackets_cycle_focus_between_panels() {
        let mut app = App::new();
        assert_eq!(app.focus, Focus::Agents);
        handle_key(&mut app, press(KeyCode::Char(']')));
        assert_eq!(app.focus, Focus::Tabs);
        handle_key(&mut app, press(KeyCode::Char(']')));
        assert_eq!(app.focus, Focus::Body);
        handle_key(&mut app, press(KeyCode::Char(']')));
        assert_eq!(app.focus, Focus::Agents);
        handle_key(&mut app, press(KeyCode::Char('[')));
        assert_eq!(app.focus, Focus::Body);
    }

    #[test]
    fn tab_key_cycles_tabs_only_when_tabs_focused() {
        let mut app = App::new();
        // Agents focus → Tab is a no-op (only [ / ] cycle panels).
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.stream, crate::stream::StreamView::Messages);

        app.focus = Focus::Tabs;
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.stream, crate::stream::StreamView::Tasks);
        handle_key(&mut app, press(KeyCode::BackTab));
        assert_eq!(app.stream, crate::stream::StreamView::Messages);
    }

    #[test]
    fn arrow_keys_scope_to_focused_panel() {
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

        handle_key(&mut app, press(KeyCode::Down));
        assert_eq!(app.selected_entry, 1, "Down on Agents focus moves cursor");

        // In Tabs focus, Down should drop into Body, not move the
        // agent cursor.
        app.focus = Focus::Tabs;
        let before = app.selected_entry;
        handle_key(&mut app, press(KeyCode::Down));
        assert_eq!(app.selected_entry, before);
        assert_eq!(app.focus, Focus::Body);
    }

    #[test]
    fn digit_keys_jump_tab_and_focus_body() {
        let mut app = App::new();
        handle_key(&mut app, press(KeyCode::Char('3')));
        assert_eq!(app.stream, crate::stream::StreamView::Tokens);
        assert_eq!(app.focus, Focus::Body);
    }
}
