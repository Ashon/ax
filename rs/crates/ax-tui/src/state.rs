//! Pure state container for the TUI. Keeping this free of ratatui
//! or IO types means we can unit-test layout and input logic without
//! a real terminal.

use std::collections::BTreeMap;
use std::time::Instant;

use ax_config::ProjectNode;
use ax_daemon::HistoryEntry;
use ax_proto::types::{Task, WorkspaceInfo};
use ax_tmux::SessionInfo;

use crate::actions::{Notice, QuickActionId, QuickActionState};

#[derive(Debug, Clone)]
pub(crate) struct PendingLifecycle {
    pub action: QuickActionId,
    pub workspace: String,
}
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
    pub(crate) tasks: Vec<Task>,
    pub(crate) task_selected: usize,
    pub(crate) quick_actions: QuickActionState,
    pub(crate) quick_notice: Option<Notice>,
    /// Lifecycle action queued by the input handler; executed by the
    /// app loop (where paths are available) and cleared.
    pub(crate) pending_lifecycle: Option<PendingLifecycle>,
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
            tasks: Vec::new(),
            task_selected: 0,
            quick_actions: QuickActionState::default(),
            quick_notice: None,
            pending_lifecycle: None,
            last_refresh: None,
            daemon_running: false,
            notice: None,
            quit: false,
        }
    }

    pub(crate) fn cycle_stream(&mut self) {
        self.stream = self.stream.next();
    }

    /// Move the task-list cursor inside the Tasks stream view. No-op
    /// when there are no tasks; clamps to valid bounds otherwise.
    pub(crate) fn move_task_selection(&mut self, delta: i32) {
        if self.tasks.is_empty() {
            self.task_selected = 0;
            return;
        }
        let n = self.tasks.len() as i32;
        let next = (self.task_selected as i32 + delta).clamp(0, n - 1) as usize;
        self.task_selected = next;
    }

    /// Called after each refresh so an out-of-range selection (tasks
    /// removed underneath the cursor) snaps back to the last live
    /// row instead of rendering nothing.
    pub(crate) fn clamp_task_selection(&mut self) {
        if self.tasks.is_empty() {
            self.task_selected = 0;
            return;
        }
        if self.task_selected >= self.tasks.len() {
            self.task_selected = self.tasks.len() - 1;
        }
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

    /// Workspace name under the sidebar cursor, if any. Returns
    /// `None` for group rows or empty sidebars.
    pub(crate) fn selected_workspace(&self) -> Option<&str> {
        self.sidebar_entries
            .get(self.selected_entry)
            .filter(|e| !e.group)
            .map(|e| e.workspace.as_str())
    }

    /// Drop the quick-action notice once its TTL has elapsed so the
    /// footer doesn't linger on a stale status message.
    pub(crate) fn expire_notice(&mut self) {
        if let Some(notice) = &self.quick_notice {
            if std::time::Instant::now() >= notice.expires_at {
                self.quick_notice = None;
            }
        }
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
    fn move_task_selection_clamps_and_no_op_on_empty() {
        let mut app = App::new();
        app.move_task_selection(5);
        assert_eq!(app.task_selected, 0);
        app.tasks = vec![mock_task(), mock_task(), mock_task()];
        app.move_task_selection(10);
        assert_eq!(app.task_selected, 2);
        app.move_task_selection(-10);
        assert_eq!(app.task_selected, 0);
    }

    #[test]
    fn clamp_task_selection_snaps_back_when_tasks_shrink() {
        let mut app = App::new();
        app.tasks = vec![mock_task(), mock_task(), mock_task()];
        app.task_selected = 2;
        app.tasks.truncate(1);
        app.clamp_task_selection();
        assert_eq!(app.task_selected, 0);
    }

    fn mock_task() -> ax_proto::types::Task {
        let now = chrono::Utc::now();
        ax_proto::types::Task {
            id: "abc".into(),
            title: "t".into(),
            description: String::new(),
            assignee: "alpha".into(),
            created_by: "orch".into(),
            parent_task_id: String::new(),
            child_task_ids: Vec::new(),
            version: 1,
            status: ax_proto::types::TaskStatus::Pending,
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
