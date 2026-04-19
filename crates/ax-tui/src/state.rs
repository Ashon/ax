//! Pure state container for the TUI. Keeping this free of ratatui
//! or IO types means we can unit-test layout and input logic without
//! a real terminal.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use ax_config::ProjectNode;
use ax_daemon::HistoryEntry;
use ax_proto::types::{Task, WorkspaceInfo};
use ax_proto::usage::WorkspaceTrend;
use ax_tmux::SessionInfo;

use crate::actions::{Notice, QuickActionId, QuickActionState};
use crate::captures::CaptureCache;
use crate::tasks::TaskFilterMode;

#[derive(Debug, Clone)]
pub(crate) struct PendingLifecycle {
    pub action: QuickActionId,
    pub workspace: String,
}
use crate::agents::AgentEntry;
use crate::stream::StreamView;

/// Which full-pane view is active. Historical — the stream variant
/// was collapsed into a regular tab (`StreamView::Stream`) so this
/// enum only retains `Grid` today. Kept as a hook in case a true
/// full-screen mode (e.g. zoom) lands later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Grid,
}

/// Which half of the body the keyboard is scoped to. `[`/`]` toggles
/// between the two; Tab/1-5 pick a tab regardless of focus. Arrow
/// keys stay *inside* the focused half so navigation never leaks
/// across panes by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    /// Top half of the body — the list for the active tab. Arrows
    /// move the per-tab selection cursor; Enter opens the tab's
    /// action surface (currently only wired on the agents tab).
    List,
    /// Bottom half of the body — the detail pane for the current
    /// list selection. Arrows scroll the detail (long task logs,
    /// wrapped message content, etc.).
    Detail,
}

impl Focus {
    pub(crate) const ORDER: [Focus; 2] = [Focus::List, Focus::Detail];

    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Focus::List => "list",
            Focus::Detail => "detail",
        }
    }
}

/// Shared cursor primitive for every stream view. Carries a single
/// `index` whose meaning is view-specific — messages read it as
/// "entries back from the tail", tokens as "first visible row",
/// tasks as "selected row index". The type stays dumb on purpose:
/// floor clamping happens here, ceiling clamping happens in render
/// where the pane height is known.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PaneCursor {
    pub(crate) index: usize,
}

impl PaneCursor {
    /// Shift the cursor by `delta`. Positive grows the index, negative
    /// shrinks it. Clamps at zero so repeated downward presses don't
    /// underflow into a follow-mode ambiguity.
    pub(crate) fn shift(&mut self, delta: i32) {
        self.index = (self.index as i32).saturating_add(delta).max(0) as usize;
    }

    /// Snap to zero — "top" for top-anchored views, "tail/follow" for
    /// tail-anchored views.
    pub(crate) fn reset(&mut self) {
        self.index = 0;
    }

    /// Jump to `idx`. Used for "other end" gestures where the caller
    /// intentionally over-shoots and lets the render pass clamp the
    /// ceiling — the cursor primitive never needs the pane height.
    pub(crate) fn jump_to(&mut self, idx: usize) {
        self.index = idx;
    }

    /// Clamp down to `ceiling` if the underlying row set shrank. Used
    /// after a refresh to keep selection from pointing past the last
    /// visible row.
    pub(crate) fn clamp(&mut self, ceiling: usize) {
        if self.index > ceiling {
            self.index = ceiling;
        }
    }
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
    pub(crate) agent_entries: Vec<AgentEntry>,
    pub(crate) selected_entry: usize,
    /// Which panel currently owns the keyboard.
    pub(crate) focus: Focus,
    pub(crate) stream: StreamView,
    pub(crate) messages: Vec<HistoryEntry>,
    /// Absolute row index of the currently-selected message (0 =
    /// oldest, `len - 1` = newest). The list highlights this row and
    /// the detail pane reads the matching entry, so list + detail
    /// stay locked together. Render clamps the ceiling against the
    /// live `messages` length.
    pub(crate) messages_cursor: PaneCursor,
    /// When `true`, new messages appearing in `messages` bump the
    /// cursor forward so the operator keeps seeing the latest entry
    /// selected. Walking back with ↑/PgUp/g breaks follow-mode;
    /// `G`/End re-engages it.
    pub(crate) messages_follow_tail: bool,
    pub(crate) tasks: Vec<Task>,
    pub(crate) task_cursor: PaneCursor,
    pub(crate) task_filter: TaskFilterMode,
    /// First visible row index in the tokens table. Clamped at render
    /// so shrinking `usage_trends` doesn't strand the view past the
    /// last row.
    pub(crate) tokens_cursor: PaneCursor,
    /// Scroll offset for the detail pane (bottom half of the body).
    /// Shared across tabs because only one detail is visible at a
    /// time; flipping tabs resets the offset so operators don't see
    /// a stray scroll carry over. Render clamps the ceiling.
    pub(crate) detail_scroll: PaneCursor,
    pub(crate) quick_actions: QuickActionState,
    pub(crate) quick_notice: Option<Notice>,
    /// Toggle for the `?` help overlay. When true, the TUI draws a
    /// centered keybinding reference over the grid and swallows all
    /// keys (except `?` / `Esc` / `q`) so the panel underneath
    /// doesn't drift.
    pub(crate) help_open: bool,
    /// Lifecycle action queued by the input handler; executed by the
    /// app loop (where paths are available) and cleared.
    pub(crate) pending_lifecycle: Option<PendingLifecycle>,
    pub(crate) captures: CaptureCache,
    /// Rolled-up per-workspace token usage returned by the daemon's
    /// `usage_trends` handler. Persists across refresh ticks so the
    /// tokens panel can still render totals for offline agents (their
    /// transcripts are on disk, not in a live tmux pane). Throttled
    /// in `app::refresh` via `last_usage_refresh`.
    pub(crate) usage_trends: BTreeMap<String, WorkspaceTrend>,
    pub(crate) last_usage_refresh: Option<Instant>,
    /// Absolute workspace-dir lookup keyed by merged workspace name.
    /// Built when the config tree reloads and consumed by the
    /// `usage_trends` request so the daemon resolves the correct
    /// Claude project dir + `CODEX_HOME` per workspace.
    pub(crate) workspace_dirs: BTreeMap<String, PathBuf>,
    pub(crate) throbber_state: throbber_widgets_tui::ThrobberState,
    /// When `Some(ws)`, the body pane becomes a full-pane live
    /// tmux capture mirror of `ws`. Pressing `esc` clears the
    /// streaming mode; pressing it again opens the quick-action
    /// overlay. Corresponds to `viewModeStream`.
    pub(crate) streamed_workspace: Option<String>,
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
            agent_entries: Vec::new(),
            selected_entry: 0,
            focus: Focus::List,
            stream: StreamView::Agents,
            messages: Vec::new(),
            messages_cursor: PaneCursor::default(),
            messages_follow_tail: true,
            tasks: Vec::new(),
            task_cursor: PaneCursor::default(),
            task_filter: TaskFilterMode::Active,
            tokens_cursor: PaneCursor::default(),
            detail_scroll: PaneCursor::default(),
            quick_actions: QuickActionState::default(),
            quick_notice: None,
            help_open: false,
            streamed_workspace: None,
            pending_lifecycle: None,
            captures: CaptureCache::default(),
            usage_trends: BTreeMap::new(),
            last_usage_refresh: None,
            workspace_dirs: BTreeMap::new(),
            throbber_state: throbber_widgets_tui::ThrobberState::default(),
            last_refresh: None,
            daemon_running: false,
            notice: None,
            quit: false,
        }
    }

    /// Step the stream tab forward (+1) or backward (-1). Used by
    /// Tab/Shift-Tab so `[/]` can stay reserved for list↔detail
    /// toggling. Resets `detail_scroll` so the new tab's detail
    /// opens at the top instead of inheriting the previous view's
    /// scroll position.
    pub(crate) fn step_stream(&mut self, delta: i32) {
        let views = StreamView::ALL;
        let current = views.iter().position(|v| *v == self.stream).unwrap_or(0);
        let n = views.len() as i32;
        let next = ((current as i32 + delta).rem_euclid(n)) as usize;
        self.stream = views[next];
        self.detail_scroll.reset();
        self.clamp_task_selection();
    }

    /// Advance the keyboard focus one panel forward (+1) or backward
    /// (-1). `[` and `]` are the only keys that touch this; the rest
    /// of the handler dispatches against `self.focus`.
    pub(crate) fn cycle_focus(&mut self, delta: i32) {
        let order = Focus::ORDER;
        let current = order
            .iter()
            .position(|f| *f == self.focus)
            .unwrap_or(0) as i32;
        let n = order.len() as i32;
        let next = current.wrapping_add(delta).rem_euclid(n) as usize;
        self.focus = order[next];
    }

    /// Jump directly to tab index `idx` from the bottom tab bar.
    /// Out-of-range indices are ignored so stray key presses don't
    /// flicker the pane. Resets `detail_scroll` for the same reason
    /// as `step_stream`.
    pub(crate) fn select_stream(&mut self, idx: usize) {
        if let Some(view) = StreamView::ALL.get(idx) {
            self.stream = *view;
            self.detail_scroll.reset();
        }
    }

    /// Filtered view of `self.tasks` using the current filter
    /// setting. The agents panel / detail pane both derive their state
    /// from this so cursor + render stay consistent.
    pub(crate) fn filtered_tasks(&self) -> Vec<Task> {
        crate::tasks::filter_tasks(&self.tasks, self.task_filter)
    }

    /// Move the task-list cursor inside the Tasks stream view. Uses
    /// the filtered list so arrow keys advance by visible rows only.
    pub(crate) fn move_task_selection(&mut self, delta: i32) {
        let filtered = self.filtered_tasks();
        if filtered.is_empty() {
            self.task_cursor.reset();
            return;
        }
        self.task_cursor.shift(delta);
        self.task_cursor.clamp(filtered.len() - 1);
    }

    /// Cycle the filter and snap the cursor so it stays valid in
    /// the new view.
    pub(crate) fn cycle_task_filter(&mut self) {
        self.task_filter = self.task_filter.next();
        self.clamp_task_selection();
    }

    /// Move the message selection by `delta` rows. Positive walks
    /// forward in time (toward the newest entry), negative walks
    /// back. Follow-tail mode re-engages only when the cursor lands
    /// exactly on the last row so a single keystroke doesn't
    /// silently flip between "parked" and "live-tailing" states.
    pub(crate) fn scroll_messages(&mut self, delta: i32) {
        if self.messages.is_empty() {
            self.messages_cursor.reset();
            self.messages_follow_tail = true;
            return;
        }
        let n = self.messages.len() as i32;
        let next = (self.messages_cursor.index as i32 + delta).clamp(0, n - 1) as usize;
        self.messages_cursor.index = next;
        self.messages_follow_tail = next + 1 == self.messages.len();
    }

    /// Re-engage live-tail: select the newest message and let future
    /// refreshes keep bumping the cursor forward.
    pub(crate) fn messages_to_tail(&mut self) {
        self.messages_follow_tail = true;
        if !self.messages.is_empty() {
            self.messages_cursor.index = self.messages.len() - 1;
        } else {
            self.messages_cursor.reset();
        }
    }

    /// Jump to the oldest message and break live-tail — operator is
    /// paging through history and doesn't want new entries to steal
    /// the selection.
    pub(crate) fn messages_to_head(&mut self) {
        self.messages_follow_tail = false;
        self.messages_cursor.reset();
    }

    /// Keep the cursor anchored to the newest message when follow-tail
    /// is on, and clamp against the current length so old selections
    /// don't dangle past a shrunken log. Called after each refresh.
    pub(crate) fn reconcile_message_cursor(&mut self) {
        if self.messages.is_empty() {
            self.messages_cursor.reset();
            self.messages_follow_tail = true;
            return;
        }
        if self.messages_follow_tail {
            self.messages_cursor.index = self.messages.len() - 1;
        } else {
            self.messages_cursor.clamp(self.messages.len() - 1);
        }
    }

    /// Shift the tokens list viewport. Positive `delta` moves the
    /// view down (toward the last row); negative moves up.
    pub(crate) fn scroll_tokens(&mut self, delta: i32) {
        self.tokens_cursor.shift(delta);
    }

    pub(crate) fn tokens_to_head(&mut self) {
        self.tokens_cursor.reset();
    }

    pub(crate) fn tokens_to_tail(&mut self) {
        self.tokens_cursor.jump_to(self.token_row_count());
    }

    /// Rows the tokens table actually renders. Mirrors the filter in
    /// `draw_tokens` so scroll bookkeeping stays in sync with what the
    /// user sees.
    fn token_row_count(&self) -> usize {
        self.usage_trends
            .values()
            .filter(|t| t.available && t.total.total() > 0)
            .count()
    }

    /// Called after each refresh so an out-of-range selection (tasks
    /// removed underneath the cursor, or filter change shrank the
    /// visible list) snaps back to the last live row.
    pub(crate) fn clamp_task_selection(&mut self) {
        let n = self.filtered_tasks().len();
        if n == 0 {
            self.task_cursor.reset();
            return;
        }
        self.task_cursor.clamp(n - 1);
    }

    /// Regenerate agents-panel entries from the current session + tree
    /// state. Callers trigger this after a refresh tick so selection
    /// stays in sync.
    pub(crate) fn rebuild_agents(&mut self) {
        self.agent_entries = crate::agents::build_entries(
            &self.sessions,
            self.tree.as_ref(),
            self.reconfigure_enabled,
            &self.desired,
        );
        let selectable = selectable_entry_positions(&self.agent_entries);
        if selectable.is_empty() {
            self.selected_entry = 0;
            return;
        }
        if !selectable.contains(&self.selected_entry) {
            self.selected_entry = selectable[0];
        }
    }

    pub(crate) fn move_selection(&mut self, delta: i32) {
        let selectable = selectable_entry_positions(&self.agent_entries);
        if selectable.is_empty() {
            self.selected_entry = 0;
            return;
        }
        let current_pos = selectable
            .iter()
            .position(|&idx| idx == self.selected_entry)
            .unwrap_or(0);
        let next = (current_pos as i32 + delta).clamp(0, selectable.len() as i32 - 1) as usize;
        self.selected_entry = selectable[next];
    }

    pub(crate) fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    /// Workspace name under the agents-panel cursor, if any. Returns
    /// `None` for group rows or empty panels.
    pub(crate) fn selected_workspace(&self) -> Option<&str> {
        self.agent_entries
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

    pub(crate) fn tick_animation(&mut self) {
        self.throbber_state.calc_next();
    }
}

/// Indexes of agents-panel entries that accept the selection cursor.
/// Group headers are skipped; offline workspace rows stay selectable
/// so the overlay (restart/stop/stream) can still target them.
fn selectable_entry_positions(entries: &[AgentEntry]) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| (!entry.group).then_some(idx))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_task_selection_clamps_and_no_op_on_empty() {
        let mut app = App::new();
        app.move_task_selection(5);
        assert_eq!(app.task_cursor.index, 0);
        app.tasks = vec![mock_task(), mock_task(), mock_task()];
        app.move_task_selection(10);
        assert_eq!(app.task_cursor.index, 2);
        app.move_task_selection(-10);
        assert_eq!(app.task_cursor.index, 0);
    }

    #[test]
    fn clamp_task_selection_snaps_back_when_tasks_shrink() {
        let mut app = App::new();
        app.tasks = vec![mock_task(), mock_task(), mock_task()];
        app.task_cursor.index = 2;
        app.tasks.truncate(1);
        app.clamp_task_selection();
        assert_eq!(app.task_cursor.index, 0);
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
    fn pane_cursor_primitive_clamps_floor_and_obeys_ceiling() {
        let mut c = PaneCursor::default();
        c.shift(5);
        assert_eq!(c.index, 5);
        c.shift(-100);
        assert_eq!(c.index, 0, "floor clamps at zero");
        c.jump_to(12);
        assert_eq!(c.index, 12, "ceiling is set verbatim (render clamps)");
        c.clamp(7);
        assert_eq!(c.index, 7, "clamp snaps down when ceiling shrinks");
        c.clamp(20);
        assert_eq!(c.index, 7, "clamp is a no-op when already under ceiling");
        c.reset();
        assert_eq!(c.index, 0);
    }

    #[test]
    fn scroll_messages_moves_absolute_cursor_and_toggles_follow_tail() {
        let mut app = App::new();
        app.messages = vec![mock_history(); 5];
        // Set the cursor at the tail so the test reflects the state
        // after a normal refresh tick (follow-tail default).
        app.reconcile_message_cursor();
        assert_eq!(app.messages_cursor.index, 4);
        assert!(app.messages_follow_tail);

        // ↑ (delta = -1) walks one step older and breaks follow-tail.
        app.scroll_messages(-1);
        assert_eq!(app.messages_cursor.index, 3);
        assert!(
            !app.messages_follow_tail,
            "moving off the tail parks the cursor"
        );

        // ↓ back to the tail re-engages follow-tail.
        app.scroll_messages(1);
        assert_eq!(app.messages_cursor.index, 4);
        assert!(app.messages_follow_tail);

        // Clamp at both ends — no under/overflow.
        app.scroll_messages(-100);
        assert_eq!(app.messages_cursor.index, 0);
        app.scroll_messages(100);
        assert_eq!(app.messages_cursor.index, 4);
    }

    #[test]
    fn messages_to_head_and_tail_flip_follow_mode() {
        let mut app = App::new();
        app.messages = vec![mock_history(); 3];
        app.messages_to_head();
        assert_eq!(app.messages_cursor.index, 0);
        assert!(!app.messages_follow_tail, "head parks the cursor");
        app.messages_to_tail();
        assert_eq!(app.messages_cursor.index, 2);
        assert!(app.messages_follow_tail, "tail re-engages follow-mode");
    }

    #[test]
    fn reconcile_message_cursor_auto_bumps_tail_but_clamps_parked() {
        let mut app = App::new();
        app.messages = vec![mock_history(); 3];
        app.reconcile_message_cursor();
        assert_eq!(app.messages_cursor.index, 2);

        // New message arrives while follow-tail is on → cursor slides
        // forward so the operator keeps seeing the latest entry.
        app.messages.push(mock_history());
        app.reconcile_message_cursor();
        assert_eq!(app.messages_cursor.index, 3);
        assert!(app.messages_follow_tail);

        // Walk back; subsequent new messages must NOT steal the
        // selection — operators mid-history shouldn't lose their place.
        app.scroll_messages(-2);
        let parked = app.messages_cursor.index;
        app.messages.push(mock_history());
        app.reconcile_message_cursor();
        assert_eq!(app.messages_cursor.index, parked);

        // If the buffer shrinks under a parked cursor, clamp down.
        app.messages.truncate(2);
        app.reconcile_message_cursor();
        assert_eq!(app.messages_cursor.index, 1);
    }

    #[test]
    fn scroll_tokens_walks_and_snaps() {
        let mut app = App::new();
        let trend = mock_trend("a");
        app.usage_trends.insert("a".into(), trend.clone());
        app.usage_trends.insert("b".into(), mock_trend("b"));

        app.scroll_tokens(3);
        assert_eq!(app.tokens_cursor.index, 3);
        app.scroll_tokens(-10);
        assert_eq!(app.tokens_cursor.index, 0, "floor clamps at top");

        app.tokens_to_tail();
        assert_eq!(app.tokens_cursor.index, 2, "tail = row count");
        app.tokens_to_head();
        assert_eq!(app.tokens_cursor.index, 0);
    }

    fn mock_history() -> HistoryEntry {
        HistoryEntry {
            timestamp: chrono::Utc::now(),
            from: "alpha".into(),
            to: "orch".into(),
            content: "hello".into(),
            task_id: String::new(),
        }
    }

    fn mock_trend(name: &str) -> ax_proto::usage::WorkspaceTrend {
        use ax_proto::usage::{Tokens, WorkspaceTrend};
        WorkspaceTrend {
            workspace: name.into(),
            available: true,
            total: Tokens {
                input: 10,
                output: 10,
                cache_read: 0,
                cache_creation: 0,
            },
            ..WorkspaceTrend::default()
        }
    }

    #[test]
    fn move_selection_clamps_to_selectable_agent_entries() {
        let mut app = App::new();
        app.sessions = vec![mock_session("a"), mock_session("b"), mock_session("c")];
        app.rebuild_agents();
        let selectable = selectable_entry_positions(&app.agent_entries);
        assert!(!selectable.is_empty());
        app.selected_entry = selectable[0];
        app.move_selection(10);
        assert_eq!(app.selected_entry, *selectable.last().unwrap());
        app.move_selection(-10);
        assert_eq!(app.selected_entry, selectable[0]);
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
