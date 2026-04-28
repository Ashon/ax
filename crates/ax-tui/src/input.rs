//! Key event → app-state transitions. Kept separate so the
//! dispatch logic can be unit-tested without a real terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::actions::{
    apply_outcomes, contextual_actions, move_overlay_selection, run_selected, task_actions,
    ActionOutcome, Notice, QuickActionId,
};
use crate::state::{AgentDetailTab, App, Focus, PendingLifecycle, PendingTaskAction};
use crate::stream::StreamView;

pub(crate) fn handle_key(app: &mut App, event: KeyEvent) {
    if event.kind == KeyEventKind::Release {
        return;
    }
    // Ctrl-c is always an emergency exit. Plain `q` remains global
    // outside text-entry modals so task titles can still contain q.
    match event.code {
        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            app.quit = true;
            return;
        }
        _ => {}
    }

    if app.orchestrator_message_form.is_some() {
        handle_orchestrator_message_form_key(app, event);
        return;
    }

    if app.create_task_form.is_some() {
        handle_create_task_form_key(app, event);
        return;
    }

    match event.code {
        KeyCode::Char('q') => {
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

    app.ensure_stream_view_visible();

    // `?` toggles the help overlay from any non-overlay context. It's
    // global so operators can reach it without first parking focus on
    // a specific panel.
    if matches!(event.code, KeyCode::Char('?')) {
        app.help_open = true;
        return;
    }

    // Global bindings — active regardless of which panel is focused.
    // Arrow keys are deliberately *not* global: each panel owns them
    // so a stray ↑/↓ can't leak across scopes. Tab/Shift-Tab follow
    // terminal accessibility convention and move focus between the
    // list/detail panels; bracket keys are contextual tab navigation
    // (focused local detail tabs first, otherwise top-level tabs).
    match event.code {
        KeyCode::Tab => {
            app.cycle_focus(1);
            return;
        }
        KeyCode::BackTab => {
            app.cycle_focus(-1);
            return;
        }
        KeyCode::Char('[') => {
            handle_bracket_tab(app, -1);
            return;
        }
        KeyCode::Char(']') => {
            handle_bracket_tab(app, 1);
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
        KeyCode::Char('m') => {
            app.open_orchestrator_message_form();
            return;
        }
        KeyCode::Char('r') => {
            app.force_refresh = true;
            app.quick_notice = Some(Notice::new("refresh requested".into(), false));
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

fn handle_bracket_tab(app: &mut App, delta: i32) {
    if app.focus == Focus::Detail && app.stream == StreamView::Agents {
        app.step_agent_detail_tab(delta);
    } else {
        app.step_stream(delta);
    }
}

fn handle_list_key(app: &mut App, event: KeyEvent) {
    // Esc from the list cycles Back-a-step inside the list scope —
    // clears any lingering notice but doesn't steal focus. Tab
    // is the move to the detail pane.
    match app.stream {
        StreamView::Agents => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Enter => open_overlay(app),
            _ => {}
        },
        StreamView::Tasks => match event.code {
            KeyCode::Char('n') => app.open_create_task_form(),
            KeyCode::Up | KeyCode::Char('k') => app.move_task_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_task_selection(1),
            KeyCode::Enter => open_task_overlay(app),
            _ => {}
        },
        // Messages is a cursor-selected list: ↑ moves the selection
        // one step older, ↓ one step newer. Landing on the tail
        // re-engages follow-mode so new messages auto-select.
        // `g`/Home jumps to the oldest; `G`/End re-follows the tail.
        StreamView::Messages => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.scroll_messages(-1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_messages(1),
            KeyCode::PageUp => app.scroll_messages(-10),
            KeyCode::PageDown => app.scroll_messages(10),
            KeyCode::Home | KeyCode::Char('g') => app.messages_to_head(),
            KeyCode::End | KeyCode::Char('G') => app.messages_to_tail(),
            _ => {}
        },
        // Tokens is a selected workspace-usage list; render keeps the
        // selected row visible.
        StreamView::Tokens => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.scroll_tokens(-1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_tokens(1),
            KeyCode::PageUp => app.scroll_tokens(-10),
            KeyCode::PageDown => app.scroll_tokens(10),
            KeyCode::Home | KeyCode::Char('g') => app.tokens_to_head(),
            KeyCode::End | KeyCode::Char('G') => app.tokens_to_tail(),
            _ => {}
        },
        // Stream defaults to live-follow, then freezes on the first
        // scroll away from the tail until the operator jumps back with
        // G/End.
        StreamView::Stream => match event.code {
            KeyCode::Up | KeyCode::Char('k') => app.scroll_stream(-1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_stream(1),
            KeyCode::PageUp => app.scroll_stream(-10),
            KeyCode::PageDown => app.scroll_stream(10),
            KeyCode::Home | KeyCode::Char('g') => app.stream_to_head(),
            KeyCode::End | KeyCode::Char('G') => app.stream_to_tail(),
            _ => {}
        },
    }
}

fn handle_detail_key(app: &mut App, event: KeyEvent) {
    if app.stream == StreamView::Agents {
        match event.code {
            KeyCode::Char('h') => {
                app.step_agent_detail_tab(-1);
                return;
            }
            KeyCode::Char('l') => {
                app.step_agent_detail_tab(1);
                return;
            }
            _ => {}
        }
        if matches!(
            app.agent_detail_tab,
            AgentDetailTab::Messages | AgentDetailTab::Activity
        ) {
            match event.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    app.agent_detail_follow_tail = false;
                    app.detail_scroll.shift(-1);
                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.detail_scroll.shift(1);
                    return;
                }
                KeyCode::PageUp => {
                    app.agent_detail_follow_tail = false;
                    app.detail_scroll.shift(-10);
                    return;
                }
                KeyCode::PageDown => {
                    app.detail_scroll.shift(10);
                    return;
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    app.agent_detail_follow_tail = false;
                    app.detail_scroll.reset();
                    return;
                }
                KeyCode::End | KeyCode::Char('G') => {
                    app.agent_detail_follow_tail = true;
                    return;
                }
                KeyCode::Esc => {
                    app.focus = Focus::List;
                    return;
                }
                _ => {}
            }
        }
    }

    match event.code {
        KeyCode::Up | KeyCode::Char('k') => app.detail_scroll.shift(-1),
        KeyCode::Down | KeyCode::Char('j') => app.detail_scroll.shift(1),
        KeyCode::PageUp => app.detail_scroll.shift(-10),
        KeyCode::PageDown => app.detail_scroll.shift(10),
        KeyCode::Home | KeyCode::Char('g') => app.detail_scroll.reset(),
        // Esc drops focus back to the list so operators can exit the
        // detail scope without hitting Tab explicitly.
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
/// Tokens moves the selected workspace-usage row.
pub(crate) fn handle_scroll(app: &mut App, direction: i32) {
    if app.quick_actions.open || app.help_open {
        // Overlays swallow the wheel so a stray scroll doesn't flicker
        // the panel underneath while a destructive confirm or the
        // help cheatsheet is visible.
        return;
    }
    app.ensure_stream_view_visible();
    match app.focus {
        Focus::List => match app.stream {
            StreamView::Agents => app.move_selection(direction),
            StreamView::Tasks => app.move_task_selection(direction),
            // Wheel-down = one step newer, wheel-up = one step older
            // (`direction` is already signed that way), so no sign
            // flip needed now that messages use absolute indices.
            StreamView::Messages => app.scroll_messages(direction),
            StreamView::Tokens => app.scroll_tokens(direction),
            StreamView::Stream => app.scroll_stream(direction),
        },
        Focus::Detail => app.detail_scroll.shift(direction),
    }
}

fn handle_create_task_form_key(app: &mut App, event: KeyEvent) {
    if app
        .create_task_form
        .as_ref()
        .is_some_and(|form| form.submitting)
    {
        return;
    }

    match event.code {
        KeyCode::Esc => {
            app.cancel_create_task_form();
        }
        KeyCode::Tab | KeyCode::Down => {
            if let Some(form) = app.create_task_form.as_mut() {
                form.step_field(1);
            }
        }
        KeyCode::BackTab | KeyCode::Up => {
            if let Some(form) = app.create_task_form.as_mut() {
                form.step_field(-1);
            }
        }
        KeyCode::Enter => {
            app.submit_create_task_form();
        }
        KeyCode::Backspace => {
            if let Some(form) = app.create_task_form.as_mut() {
                form.active_value_mut().pop();
                form.error = None;
            }
        }
        KeyCode::Delete => {
            if let Some(form) = app.create_task_form.as_mut() {
                form.active_value_mut().clear();
                form.error = None;
            }
        }
        KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(form) = app.create_task_form.as_mut() {
                form.active_value_mut().clear();
                form.error = None;
            }
        }
        KeyCode::Char(c)
            if !event
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            if let Some(form) = app.create_task_form.as_mut() {
                form.active_value_mut().push(c);
                form.error = None;
            }
        }
        _ => {}
    }
}

fn handle_orchestrator_message_form_key(app: &mut App, event: KeyEvent) {
    if app
        .orchestrator_message_form
        .as_ref()
        .is_some_and(|form| form.submitting)
    {
        return;
    }

    match event.code {
        KeyCode::Esc => {
            app.cancel_orchestrator_message_form();
        }
        KeyCode::Enter => {
            app.submit_orchestrator_message_form();
        }
        KeyCode::Backspace => {
            if let Some(form) = app.orchestrator_message_form.as_mut() {
                form.message.pop();
                form.error = None;
            }
        }
        KeyCode::Delete => {
            if let Some(form) = app.orchestrator_message_form.as_mut() {
                form.message.clear();
                form.error = None;
            }
        }
        KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(form) = app.orchestrator_message_form.as_mut() {
                form.message.clear();
                form.error = None;
            }
        }
        KeyCode::Char(c)
            if !event
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            if let Some(form) = app.orchestrator_message_form.as_mut() {
                form.message.push(c);
                form.error = None;
            }
        }
        _ => {}
    }
}

fn open_overlay(app: &mut App) {
    if app.selected_workspace().is_none() {
        app.quick_notice = Some(Notice::new("No workspace selected".into(), true));
        return;
    }
    let has_session = app
        .agent_entries
        .get(app.selected_entry)
        .is_some_and(|entry| entry.session_index.is_some());
    app.quick_actions.actions = contextual_actions(has_session);
    app.quick_notice = None;
    app.quick_actions.target_workspace = app.selected_workspace().unwrap_or("").to_owned();
    app.quick_actions.target_task_id.clear();
    app.quick_actions.target_task_version = 0;
    app.quick_actions.selected = 0;
    app.quick_actions.confirm = false;
    app.quick_actions.open = true;
}

fn open_task_overlay(app: &mut App) {
    let Some(task) = app.selected_task() else {
        app.quick_notice = Some(Notice::new("No task selected".into(), true));
        return;
    };
    let actions = task_actions(&task);
    if actions.is_empty() {
        app.quick_notice = Some(Notice::new(
            format!(
                "No remediation actions for task {}",
                crate::tasks::short_task_id(&task.id)
            ),
            true,
        ));
        return;
    }
    app.quick_actions.actions = actions;
    app.quick_actions.selected = 0;
    app.quick_actions.confirm = false;
    app.quick_actions.open = true;
    app.quick_actions.target_workspace.clear();
    app.quick_actions.target_task_id = task.id;
    app.quick_actions.target_task_version = task.version;
    app.quick_notice = None;
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
    let Some(action) = app.quick_actions.current() else {
        return;
    };
    if action.id.is_task_action() {
        if action.id.requires_confirmation() && !app.quick_actions.confirm {
            apply_outcomes(app, vec![ActionOutcome::NeedsConfirm]);
            return;
        }
        let task_id = app.quick_actions.target_task_id.clone();
        if task_id.is_empty() {
            app.quick_actions.open = false;
            return;
        }
        app.pending_task_action = Some(PendingTaskAction {
            action: action.id,
            task_id: task_id.clone(),
            expected_version: app.quick_actions.target_task_version,
        });
        app.quick_actions.open = false;
        app.quick_actions.confirm = false;
        app.quick_notice = Some(Notice::new(
            format!(
                "{} requested for {}",
                action.id.label(),
                crate::tasks::short_task_id(&task_id)
            ),
            false,
        ));
        return;
    }
    let Some(target) = app.selected_workspace().map(str::to_owned) else {
        app.quick_actions.open = false;
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
            workspace: target.clone(),
        });
        app.quick_notice = Some(Notice::new(
            format!("{} requested for {}", action.id.label(), target),
            false,
        ));
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
    use ax_proto::types::{Task, TaskStartMode, TaskStatus};
    use ax_proto::usage::{Tokens, WorkspaceTrend};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mock_history() -> ax_daemon::HistoryEntry {
        ax_daemon::HistoryEntry {
            timestamp: chrono::Utc::now(),
            from: "alpha".into(),
            to: "orch".into(),
            content: "hi".into(),
            task_id: String::new(),
        }
    }

    fn mock_trend(name: &str) -> WorkspaceTrend {
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

    fn mock_task(id: &str, status: TaskStatus) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: id.into(),
            title: "task".into(),
            description: String::new(),
            assignee: "alpha".into(),
            created_by: "orch".into(),
            parent_task_id: String::new(),
            child_task_ids: Vec::new(),
            version: 7,
            status,
            start_mode: TaskStartMode::Default,
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
    fn enter_opens_overlay_when_workspace_selected() {
        let mut app = App::new();
        // No agents panel = no workspace → enter is a no-op.
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(!app.quick_actions.open);
        assert!(app.quick_notice.is_some());

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
    fn enter_on_offline_workspace_hides_session_only_actions() {
        let mut app = App::new();
        app.agent_entries = vec![crate::agents::AgentEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: None,
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        handle_key(&mut app, press(KeyCode::Enter));
        let actions: Vec<_> = app.quick_actions.actions.iter().map(|a| a.id).collect();
        assert!(!actions.contains(&QuickActionId::StreamTmux));
        assert!(!actions.contains(&QuickActionId::Interrupt));
        assert!(!actions.contains(&QuickActionId::Stop));
        assert!(actions.contains(&QuickActionId::Restart));
    }

    #[test]
    fn enter_on_task_opens_remediation_overlay_and_queues_confirmed_action() {
        let mut app = App::new();
        app.stream = StreamView::Tasks;
        app.tasks = vec![mock_task("abcdef123456", TaskStatus::InProgress)];

        handle_key(&mut app, press(KeyCode::Enter));
        assert!(app.quick_actions.open);
        assert_eq!(app.quick_actions.target_task_id, "abcdef123456");
        let ids: Vec<_> = app.quick_actions.actions.iter().map(|a| a.id).collect();
        assert!(ids.contains(&QuickActionId::TaskWake));
        assert!(ids.contains(&QuickActionId::TaskInterrupt));
        assert!(ids.contains(&QuickActionId::TaskRetry));
        assert!(ids.contains(&QuickActionId::TaskCancel));

        app.quick_actions.selected = app
            .quick_actions
            .actions
            .iter()
            .position(|a| a.id == QuickActionId::TaskCancel)
            .expect("cancel action");
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(app.quick_actions.confirm);
        assert!(app.pending_task_action.is_none());

        handle_key(&mut app, press(KeyCode::Enter));
        let pending = app.pending_task_action.expect("queued task action");
        assert_eq!(pending.action, QuickActionId::TaskCancel);
        assert_eq!(pending.task_id, "abcdef123456");
        assert_eq!(pending.expected_version, 7);
        assert!(!app.quick_actions.open);
    }

    #[test]
    fn n_on_tasks_opens_create_form_and_enter_queues_create() {
        let mut app = App::new();
        app.stream = StreamView::Tasks;
        app.workspace_dirs
            .insert("alpha".into(), std::path::PathBuf::from("/tmp/alpha"));

        handle_key(&mut app, press(KeyCode::Char('n')));
        assert!(app.create_task_form.is_some());
        assert_eq!(
            app.create_task_form.as_ref().unwrap().assignee,
            "alpha",
            "configured workspace should prefill assignee"
        );

        for c in "Build queue".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Tab));
        for c in "Wire top create form".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));

        let pending = app.pending_task_create.expect("create queued");
        assert_eq!(pending.draft.assignee, "alpha");
        assert_eq!(pending.draft.title, "Build queue");
        assert_eq!(pending.draft.description, "Wire top create form");
        assert!(app.create_task_form.as_ref().unwrap().submitting);
    }

    #[test]
    fn m_opens_orchestrator_message_form_and_enter_queues_send() {
        let mut app = App::new();

        handle_key(&mut app, press(KeyCode::Char('m')));
        assert!(app.orchestrator_message_form.is_some());

        for c in "Check queue health".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));

        let pending = app
            .pending_orchestrator_message
            .expect("message send queued");
        assert_eq!(pending.message, "Check queue health");
        assert!(app.orchestrator_message_form.as_ref().unwrap().submitting);
    }

    #[test]
    fn orchestrator_message_form_swallow_plain_q_but_ctrl_c_still_quits() {
        let mut app = App::new();
        app.open_orchestrator_message_form();

        handle_key(&mut app, press(KeyCode::Char('q')));
        assert!(!app.quit);
        assert_eq!(app.orchestrator_message_form.as_ref().unwrap().message, "q");

        let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        handle_key(&mut app, event);
        assert!(app.quit);
    }

    #[test]
    fn task_create_form_swallow_plain_q_but_ctrl_c_still_quits() {
        let mut app = App::new();
        app.stream = StreamView::Tasks;
        app.open_create_task_form();

        handle_key(&mut app, press(KeyCode::Char('q')));
        assert!(!app.quit);
        assert_eq!(app.create_task_form.as_ref().unwrap().title, "q");

        let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        handle_key(&mut app, event);
        assert!(app.quit);
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
    fn r_requests_immediate_refresh() {
        let mut app = App::new();
        handle_key(&mut app, press(KeyCode::Char('r')));
        assert!(app.force_refresh);
        assert!(app
            .quick_notice
            .as_ref()
            .is_some_and(|notice| notice.text.contains("refresh")));
    }

    #[test]
    fn tab_and_backtab_toggle_focus_between_panels() {
        let mut app = App::new();
        assert_eq!(app.stream, StreamView::Agents);
        assert_eq!(app.focus, Focus::List);
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Detail);
        assert_eq!(
            app.stream,
            StreamView::Agents,
            "Tab moves focus without switching top-level tabs"
        );
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.focus, Focus::List);
        handle_key(&mut app, press(KeyCode::BackTab));
        assert_eq!(app.focus, Focus::Detail);
        assert_eq!(
            app.stream,
            StreamView::Agents,
            "Shift-Tab moves focus backward without switching top-level tabs"
        );
        handle_key(&mut app, press(KeyCode::BackTab));
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn bracket_keys_cycle_tabs_from_any_focus() {
        let mut app = App::new();
        // Default is Agents; ] walks to Messages without stealing
        // the List↔Detail focus so operators can peek at other tabs.
        assert_eq!(app.stream, crate::stream::StreamView::Agents);
        handle_key(&mut app, press(KeyCode::Char(']')));
        assert_eq!(app.stream, crate::stream::StreamView::Messages);
        assert_eq!(app.focus, Focus::List);

        app.focus = Focus::Detail;
        handle_key(&mut app, press(KeyCode::Char('[')));
        assert_eq!(app.stream, crate::stream::StreamView::Agents);
        assert_eq!(app.focus, Focus::Detail);
    }

    #[test]
    fn bracket_keys_prefer_focused_agent_detail_tabs() {
        let mut app = App::new();
        app.stream = StreamView::Agents;
        app.focus = Focus::Detail;
        assert_eq!(app.agent_detail_tab, AgentDetailTab::Overview);

        handle_key(&mut app, press(KeyCode::Char(']')));
        assert_eq!(app.stream, StreamView::Agents);
        assert_eq!(app.agent_detail_tab, AgentDetailTab::Tasks);

        handle_key(&mut app, press(KeyCode::Char('[')));
        assert_eq!(app.stream, StreamView::Agents);
        assert_eq!(app.agent_detail_tab, AgentDetailTab::Overview);
    }

    #[test]
    fn bracket_keys_switch_top_level_tabs_when_detail_has_no_local_tabs() {
        let mut app = App::new();
        app.stream = StreamView::Tasks;
        app.focus = Focus::Detail;
        app.agent_detail_tab = AgentDetailTab::Messages;

        handle_key(&mut app, press(KeyCode::Char(']')));
        assert_eq!(app.stream, StreamView::Tokens);
        assert_eq!(
            app.agent_detail_tab,
            AgentDetailTab::Messages,
            "non-agent detail panes do not consume bracket keys"
        );
    }

    #[test]
    fn agent_detail_h_l_cycles_local_tabs_only_in_agents_detail_focus() {
        let mut app = App::new();
        app.focus = Focus::Detail;
        assert_eq!(app.agent_detail_tab, crate::state::AgentDetailTab::Overview);

        handle_key(&mut app, press(KeyCode::Char('l')));
        assert_eq!(app.stream, StreamView::Agents);
        assert_eq!(app.agent_detail_tab, crate::state::AgentDetailTab::Tasks);
        assert_eq!(app.focus, Focus::Detail);

        handle_key(&mut app, press(KeyCode::Char('h')));
        assert_eq!(app.agent_detail_tab, crate::state::AgentDetailTab::Overview);

        app.focus = Focus::List;
        handle_key(&mut app, press(KeyCode::Char('l')));
        assert_eq!(
            app.agent_detail_tab,
            crate::state::AgentDetailTab::Overview,
            "list focus does not switch local detail tabs"
        );

        app.focus = Focus::Detail;
        app.stream = StreamView::Messages;
        handle_key(&mut app, press(KeyCode::Char('l')));
        assert_eq!(
            app.agent_detail_tab,
            crate::state::AgentDetailTab::Overview,
            "non-agents detail panes do not consume agent local tab keys"
        );
    }

    #[test]
    fn agent_detail_time_tabs_use_sticky_tail_until_user_scrolls_away() {
        let mut app = App::new();
        app.focus = Focus::Detail;
        app.stream = StreamView::Agents;
        app.agent_detail_tab = AgentDetailTab::Messages;
        app.detail_scroll.index = 5;
        app.agent_detail_follow_tail = true;

        handle_key(&mut app, press(KeyCode::Up));
        assert!(!app.agent_detail_follow_tail);
        assert_eq!(app.detail_scroll.index, 4);

        handle_key(&mut app, press(KeyCode::Char('G')));
        assert!(app.agent_detail_follow_tail);

        app.agent_detail_tab = AgentDetailTab::Activity;
        handle_key(&mut app, press(KeyCode::Char('g')));
        assert!(!app.agent_detail_follow_tail);
        assert_eq!(app.detail_scroll.index, 0);
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
        // `4` is Tokens (Agents=1, Messages=2, Tasks=3, Tokens=4).
        // Stream is hidden until a workspace is pinned, so `5` is
        // ignored on a cold start.
        handle_key(&mut app, press(KeyCode::Char('4')));
        assert_eq!(app.stream, crate::stream::StreamView::Tokens);
        assert_eq!(app.focus, Focus::List, "digit keys preserve current focus");
        handle_key(&mut app, press(KeyCode::Char('5')));
        assert_eq!(app.stream, crate::stream::StreamView::Tokens);

        // Once a workspace is pinned, the contextual Stream view
        // becomes visible as the 5th slot.
        app.streamed_workspace = Some("alpha".into());
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

        // List + Messages: populate a tail-selected cursor, then
        // wheel-up (direction=-1) walks one message older. The cursor
        // is an absolute index now, so direction feeds through
        // without inversion.
        app.stream = StreamView::Messages;
        app.messages = vec![mock_history(); 3];
        app.reconcile_message_cursor();
        assert_eq!(app.messages_cursor.index, 2, "tail-selected by default");
        handle_scroll(&mut app, -1);
        assert_eq!(app.messages_cursor.index, 1);
        handle_scroll(&mut app, 1);
        assert_eq!(app.messages_cursor.index, 2);

        // List + Tokens: wheel-down moves selection toward the last row.
        app.stream = StreamView::Tokens;
        app.usage_trends.insert("a".into(), mock_trend("a"));
        app.usage_trends.insert("b".into(), mock_trend("b"));
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

    #[test]
    fn stream_keys_scroll_and_restore_follow_tail() {
        let mut app = App::new();
        app.stream = StreamView::Stream;
        app.streamed_workspace = Some("alpha".into());
        app.stream_cursor.index = 20;
        assert!(app.stream_follow_tail);

        handle_key(&mut app, press(KeyCode::Up));
        assert_eq!(app.stream_cursor.index, 19);
        assert!(!app.stream_follow_tail);

        handle_key(&mut app, press(KeyCode::Char('G')));
        assert!(app.stream_follow_tail);
        assert_eq!(app.stream_cursor.index, 0);
    }
}
