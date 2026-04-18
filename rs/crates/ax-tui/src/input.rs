//! Key event → app-state transitions. Kept separate so the
//! dispatch logic can be unit-tested without a real terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::state::App;

pub(crate) fn handle_key(app: &mut App, event: KeyEvent) {
    if event.kind == KeyEventKind::Release {
        return;
    }
    match event.code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            app.quit = true;
        }
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
