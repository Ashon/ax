//! Pure state container for the TUI. Keeping this free of ratatui
//! or IO types means we can unit-test layout and input logic without
//! a real terminal.

use std::collections::BTreeMap;
use std::time::Instant;

use ax_config::ProjectNode;
use ax_daemon::HistoryEntry;
use ax_proto::types::WorkspaceInfo;
use ax_tmux::SessionInfo;

use crate::sidebar::SidebarEntry;
use crate::stream::StreamView;

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
    pub(crate) tree: Option<ProjectNode>,
    pub(crate) reconfigure_enabled: bool,
    pub(crate) desired: BTreeMap<String, bool>,
    pub(crate) sidebar_entries: Vec<SidebarEntry>,
    pub(crate) selected_entry: usize,
    pub(crate) stream: StreamView,
    pub(crate) messages: Vec<HistoryEntry>,
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
            tree: None,
            reconfigure_enabled: false,
            desired: BTreeMap::new(),
            sidebar_entries: Vec::new(),
            selected_entry: 0,
            stream: StreamView::Messages,
            messages: Vec::new(),
            last_refresh: None,
            daemon_running: false,
            notice: None,
            quit: false,
        }
    }

    pub(crate) fn cycle_stream(&mut self) {
        self.stream = self.stream.next();
    }

    /// Regenerate sidebar entries from the current session + tree
    /// state. Callers trigger this after a refresh tick so selection
    /// stays in sync.
    pub(crate) fn rebuild_sidebar(&mut self) {
        self.sidebar_entries = crate::sidebar::build_entries(
            &self.sessions,
            self.tree.as_ref(),
            self.reconfigure_enabled,
            &self.desired,
        );
        let live = live_entry_positions(&self.sidebar_entries);
        if live.is_empty() {
            self.selected_entry = 0;
            return;
        }
        // Keep the cursor parked on a selectable row after the rebuild.
        if !live.contains(&self.selected_entry) {
            self.selected_entry = live[0];
        }
    }

    pub(crate) fn move_selection(&mut self, delta: i32) {
        let live = live_entry_positions(&self.sidebar_entries);
        if live.is_empty() {
            self.selected_entry = 0;
            return;
        }
        let current_pos = live
            .iter()
            .position(|&idx| idx == self.selected_entry)
            .unwrap_or(0);
        let next = (current_pos as i32 + delta).clamp(0, live.len() as i32 - 1) as usize;
        self.selected_entry = live[next];
    }

    pub(crate) fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }
}

/// Indexes of sidebar entries that accept the selection cursor
/// (groups and offline rows don't move the cursor).
fn live_entry_positions(entries: &[SidebarEntry]) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| (!entry.group && entry.session_index.is_some()).then_some(idx))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_selection_clamps_to_live_sidebar_entries() {
        let mut app = App::new();
        app.sessions = vec![mock_session("a"), mock_session("b"), mock_session("c")];
        app.rebuild_sidebar();
        let live = live_entry_positions(&app.sidebar_entries);
        assert!(!live.is_empty());
        app.selected_entry = live[0];
        app.move_selection(10);
        assert_eq!(app.selected_entry, *live.last().unwrap());
        app.move_selection(-10);
        assert_eq!(app.selected_entry, live[0]);
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
