//! Pure state container for the TUI. Keeping this free of ratatui
//! or IO types means we can unit-test layout and input logic without
//! a real terminal.

use std::collections::BTreeMap;
use std::time::Instant;

use ax_proto::types::WorkspaceInfo;
use ax_tmux::SessionInfo;

/// Which full-pane view is active. Currently only `Grid` is
/// implemented; `Stream` follows in the next slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Grid,
    #[allow(dead_code)]
    Stream,
}

#[derive(Debug, Clone)]
pub(crate) struct App {
    #[allow(dead_code)]
    pub(crate) view_mode: ViewMode,
    pub(crate) sessions: Vec<SessionInfo>,
    pub(crate) workspace_infos: BTreeMap<String, WorkspaceInfo>,
    pub(crate) selected: usize,
    pub(crate) last_refresh: Option<Instant>,
    pub(crate) daemon_running: bool,
    pub(crate) notice: Option<String>,
    pub(crate) quit: bool,
}

impl App {
    pub(crate) fn new() -> Self {
        Self {
            view_mode: ViewMode::Grid,
            sessions: Vec::new(),
            workspace_infos: BTreeMap::new(),
            selected: 0,
            last_refresh: None,
            daemon_running: false,
            notice: None,
            quit: false,
        }
    }

    pub(crate) fn move_selection(&mut self, delta: i32) {
        if self.sessions.is_empty() {
            self.selected = 0;
            return;
        }
        let n = self.sessions.len() as i32;
        let mut next = self.selected as i32 + delta;
        if next < 0 {
            next = 0;
        }
        if next >= n {
            next = n - 1;
        }
        self.selected = next as usize;
    }

    pub(crate) fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_selection_clamps_to_session_bounds() {
        let mut app = App::new();
        app.sessions = vec![mock_session("a"), mock_session("b"), mock_session("c")];
        app.move_selection(5);
        assert_eq!(app.selected, 2);
        app.move_selection(-10);
        assert_eq!(app.selected, 0);
    }

    fn mock_session(name: &str) -> SessionInfo {
        SessionInfo {
            name: format!("ax-{name}"),
            workspace: name.into(),
            attached: false,
            windows: 1,
        }
    }
}
