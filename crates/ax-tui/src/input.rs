//! Key event → app-state transitions. Kept separate so the
//! dispatch logic can be unit-tested without a real terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::actions::{
    apply_outcomes, default_actions, move_overlay_selection, run_selected, ActionOutcome,
    QuickActionId,
};
use crate::state::{App, PendingLifecycle};

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
    // first. All other keys still steer the sidebar + stream view so
    // you can e.g. swap to the tasks table while a capture is open.
    if app.streamed_workspace.is_some() && matches!(event.code, KeyCode::Esc) {
        app.streamed_workspace = None;
        return;
    }

    match event.code {
        KeyCode::Esc => open_overlay(app),
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Tab | KeyCode::Char('s') => app.cycle_stream(),
        KeyCode::Char('[' | 'H') => app.move_task_selection(-1),
        KeyCode::Char(']' | 'L') => app.move_task_selection(1),
        KeyCode::Char('f') => app.cycle_task_filter(),
        _ => {}
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
    fn esc_opens_overlay_when_workspace_selected() {
        let mut app = App::new();
        // No sidebar = no workspace → esc is a no-op.
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(!app.quick_actions.open);

        app.sidebar_entries = vec![crate::sidebar::SidebarEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: Some(0),
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        app.selected_entry = 0;
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(app.quick_actions.open);
        assert!(!app.quick_actions.actions.is_empty());
    }

    #[test]
    fn overlay_enter_on_restart_sets_needs_confirm_then_queues_lifecycle() {
        let mut app = App::new();
        app.sidebar_entries = vec![crate::sidebar::SidebarEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: Some(0),
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        app.selected_entry = 0;
        handle_key(&mut app, press(KeyCode::Esc));
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
}
