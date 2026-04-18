//! ratatui draw routine. Splits the screen into a sidebar (project
//! tree + live sessions) and a body pane. The body still shows a
//! selection summary; the stream view / tmux captures land in later
//! slices.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use ax_proto::types::AgentStatus;

use crate::actions::QuickActionId;
use crate::sidebar::SidebarEntry;
use crate::state::App;
use crate::stream::{format_message_line, StreamView};

const SIDEBAR_WIDTH: u16 = 34;

pub(crate) fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(1),    // body split
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(f, chunks[0], app);

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(1)])
        .split(chunks[1]);
    draw_sidebar(f, body_chunks[0], app);
    draw_body(f, body_chunks[1], app);

    if app.quick_actions.open {
        draw_quick_actions(f, body_chunks[1], app);
    }

    draw_footer(f, chunks[2], app);
}

fn draw_quick_actions(f: &mut Frame, body: Rect, app: &App) {
    let workspace = app.selected_workspace().unwrap_or("");
    let action_count = app.quick_actions.actions.len() as u16;
    let height = (action_count + 4).min(body.height.saturating_sub(2)).max(4);
    let width: u16 = 42;
    let width = width.min(body.width.saturating_sub(2));
    let x = body.x + body.width.saturating_sub(width) / 2;
    let y = body.y + body.height.saturating_sub(height) / 2;
    let area = Rect::new(x, y, width, height);
    f.render_widget(ratatui::widgets::Clear, area);

    let title = format!(" {workspace} actions ");
    let block = Block::default().borders(Borders::ALL).title(title);

    let mut lines: Vec<Line> = Vec::with_capacity(app.quick_actions.actions.len() + 2);
    if app.quick_actions.confirm {
        let action = app.quick_actions.current().map(|a| a.id);
        let prompt = action.map_or("", QuickActionId::confirm_prompt);
        lines.push(Line::from(Span::styled(
            prompt,
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "enter to confirm · esc to cancel",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for (idx, action) in app.quick_actions.actions.iter().enumerate() {
            let cursor = if idx == app.quick_actions.selected {
                "▸ "
            } else {
                "  "
            };
            let style = if idx == app.quick_actions.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!("{cursor}{}", action.id.label()),
                style,
            )));
        }
        lines.push(Line::from(Span::styled(
            "enter to run · esc to close",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let daemon = if app.daemon_running {
        "running"
    } else {
        "stopped"
    };
    let text = format!(
        "ax watch — daemon: {daemon} · agents: {} · sessions: {}",
        app.workspace_infos.len(),
        app.sessions.len(),
    );
    let header = Paragraph::new(text).style(Style::default().add_modifier(Modifier::BOLD));
    f.render_widget(header, area);
}

fn draw_sidebar(f: &mut Frame, area: Rect, app: &App) {
    if app.sidebar_entries.is_empty() {
        let empty = Paragraph::new(
            "No active agents. Run `ax up` in a project directory with .ax/config.yaml.",
        )
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(" agents "));
        f.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = app
        .sidebar_entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| sidebar_item(idx, entry, app))
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(" agents "));
    f.render_widget(list, area);
}

fn sidebar_item<'a>(idx: usize, entry: &'a SidebarEntry, app: &'a App) -> ListItem<'a> {
    let indent = "  ".repeat(entry.level);
    if entry.group {
        let span = Span::styled(
            format!("{indent}{}", entry.label),
            Style::default().add_modifier(Modifier::BOLD),
        );
        return ListItem::new(Line::from(span));
    }

    let live = entry.session_index.is_some();
    let is_selected = idx == app.selected_entry;
    let marker = if live { "●" } else { "○" };
    let marker_style = if live {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let cursor = if is_selected { "▸ " } else { "  " };
    let name_style = if is_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else if live {
        Style::default()
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let agent_state = app
        .workspace_infos
        .get(&entry.workspace)
        .map_or("offline", |ws| agent_status_str(&ws.status));
    let reconcile_label = if entry.reconcile.is_empty() {
        agent_state
    } else {
        entry.reconcile.as_str()
    };

    let spans = vec![
        Span::raw(cursor.to_string()),
        Span::raw(indent),
        Span::styled(format!("{marker} "), marker_style),
        Span::styled(entry.label.clone(), name_style),
        Span::raw("  "),
        Span::styled(
            reconcile_label.to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ];
    ListItem::new(Line::from(spans))
}

fn draw_body(f: &mut Frame, area: Rect, app: &App) {
    match app.stream {
        StreamView::Messages => draw_messages(f, area, app),
        StreamView::Tasks => draw_tasks(f, area, app),
        StreamView::Tokens => draw_stub_view(f, area, app),
        StreamView::Hidden => draw_selection_summary(f, area, app),
    }
}

fn draw_messages(f: &mut Frame, area: Rect, app: &App) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(StreamView::Messages.title());

    if app.messages.is_empty() {
        let para = Paragraph::new("  (no messages yet)")
            .style(Style::default().add_modifier(Modifier::DIM))
            .block(block);
        f.render_widget(para, area);
        return;
    }

    // Show the tail that fits inside the pane.
    let start = app.messages.len().saturating_sub(inner_height.max(1));
    let lines: Vec<Line> = app.messages[start..]
        .iter()
        .map(|entry| Line::from(Span::raw(format_message_line(entry, inner_width.max(1)))))
        .collect();
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);
}

fn draw_stub_view(f: &mut Frame, area: Rect, app: &App) {
    let text = format!(
        "{} view lands in a follow-up slice — press Tab / s to cycle back to messages",
        match app.stream {
            StreamView::Tasks => "tasks",
            StreamView::Tokens => "tokens",
            _ => "stream",
        }
    );
    let para = Paragraph::new(text)
        .wrap(Wrap { trim: true })
        .style(Style::default().add_modifier(Modifier::DIM))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(app.stream.title()),
        );
    f.render_widget(para, area);
}

fn draw_tasks(f: &mut Frame, area: Rect, app: &App) {
    let filtered = app.filtered_tasks();
    if app.tasks.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(StreamView::Tasks.title());
        let para = Paragraph::new("  (no tasks yet)")
            .style(Style::default().add_modifier(Modifier::DIM))
            .block(block);
        f.render_widget(para, area);
        return;
    }

    // Split horizontally: list ~45% / detail ~55%, clamped so neither
    // pane gets uselessly narrow on tiny terminals.
    let list_width = area
        .width
        .saturating_mul(45)
        .saturating_div(100)
        .clamp(36, area.width.saturating_sub(28));
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(list_width), Constraint::Min(24)])
        .split(area);
    draw_tasks_list(f, chunks[0], app, &filtered);
    draw_task_detail(f, chunks[1], app, &filtered);
}

fn draw_tasks_list(f: &mut Frame, area: Rect, app: &App, filtered: &[ax_proto::types::Task]) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let block = Block::default().borders(Borders::ALL).title(format!(
        " tasks {} {}/{} ",
        app.task_filter.label(),
        filtered.len().min(app.task_selected.saturating_add(1)),
        filtered.len(),
    ));

    let summary = crate::tasks::summarize_tasks(&app.tasks);
    let mut lines: Vec<Line> = Vec::with_capacity(inner_height);
    lines.push(Line::from(Span::styled(
        crate::tasks::truncate(
            &format!("Summary: {}", crate::tasks::format_task_summary(&summary)),
            inner_width.max(1),
        ),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        crate::tasks::truncate(
            "ID       PRI      STATUS          AGE    ASSIGNEE        TITLE",
            inner_width.max(1),
        ),
        Style::default().add_modifier(Modifier::DIM),
    )));

    if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no tasks match current filter — press f to cycle)",
            Style::default().add_modifier(Modifier::DIM),
        )));
        let para = Paragraph::new(lines).block(block);
        f.render_widget(para, area);
        return;
    }

    let body_budget = inner_height.saturating_sub(lines.len());
    let (start, end) = viewport_range(filtered.len(), app.task_selected, body_budget);
    for (idx, task) in filtered[start..end].iter().enumerate() {
        let absolute = start + idx;
        let style = if absolute == app.task_selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format_task_row(task, inner_width),
            style,
        )));
    }
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);
}

fn draw_task_detail(f: &mut Frame, area: Rect, app: &App, filtered: &[ax_proto::types::Task]) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let block = Block::default().borders(Borders::ALL).title(" detail ");

    let Some(task) = filtered.get(app.task_selected) else {
        let para = Paragraph::new("  (no task selected)")
            .style(Style::default().add_modifier(Modifier::DIM))
            .block(block);
        f.render_widget(para, area);
        return;
    };

    let lines = build_detail_lines(task, &app.messages, inner_width, inner_height);
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);
}

/// Walk `tasks[..]` around `selected` so the cursor stays visible
/// once the list outgrows the pane. Mirrors Go's
/// `computeTaskListViewport` at height=1 row per task.
fn viewport_range(total: usize, selected: usize, budget: usize) -> (usize, usize) {
    if total == 0 || budget == 0 {
        return (0, 0);
    }
    let visible = budget.min(total);
    let start = if selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let max_start = total - visible;
    let start = start.min(max_start);
    (start, start + visible)
}

fn format_task_row(task: &ax_proto::types::Task, width: usize) -> String {
    let id = crate::tasks::short_task_id(&task.id);
    let row = format!(
        "{id:<8} {:<8} {:<15} {:<6} {:<15} {}",
        crate::tasks::truncate(crate::tasks::task_priority_label(task.priority.as_ref()), 8),
        crate::tasks::truncate(&crate::tasks::task_status_label(task), 15),
        crate::tasks::format_task_age(task),
        crate::tasks::truncate(&task.assignee, 15),
        task.title,
    );
    crate::tasks::truncate(&row, width.max(1))
}

/// Render the right-hand detail pane. Line count caps to `height`
/// so the paragraph widget never clips mid-line.
fn build_detail_lines<'a>(
    task: &'a ax_proto::types::Task,
    history: &[ax_daemon::HistoryEntry],
    width: usize,
    height: usize,
) -> Vec<Line<'a>> {
    use crate::tasks::{
        format_task_age, task_is_stale, task_priority_label, task_status_label, truncate,
    };

    let stale_flag = if task_is_stale(task) { "yes" } else { "no" };
    let mut out: Vec<Line<'a>> = Vec::new();
    let push = |v: &mut Vec<Line<'a>>, text: String, dim: bool| {
        let line_style = if dim {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default()
        };
        v.push(Line::from(Span::styled(
            truncate(&text, width.max(1)),
            line_style,
        )));
    };

    push(&mut out, task.title.clone(), false);
    push(
        &mut out,
        format!("status: {}", task_status_label(task)),
        true,
    );
    push(&mut out, format!("version: {}", task.version), true);
    push(&mut out, format!("assignee: {}", task.assignee), true);
    push(&mut out, format!("created_by: {}", task.created_by), true);
    push(
        &mut out,
        format!("priority: {}", task_priority_label(task.priority.as_ref())),
        true,
    );
    push(
        &mut out,
        format!("updated: {} ago", format_task_age(task)),
        true,
    );
    push(&mut out, format!("stale: {stale_flag}"), true);
    if task.stale_after_seconds > 0 {
        push(
            &mut out,
            format!("stale_after: {}s", task.stale_after_seconds),
            true,
        );
    }
    if let Some(ts) = task.removed_at {
        push(
            &mut out,
            format!("removed: {}", ts.format("%Y-%m-%d %H:%M:%S")),
            true,
        );
        if !task.removed_by.is_empty() {
            push(&mut out, format!("removed_by: {}", task.removed_by), true);
        }
    }
    if !task.description.is_empty() {
        out.push(Line::from(""));
        push(&mut out, format!("desc: {}", task.description), false);
    }
    if !task.result.is_empty() {
        out.push(Line::from(""));
        push(&mut out, format!("result: {}", task.result), false);
    }
    if let Some(info) = &task.stale_info {
        out.push(Line::from(""));
        push(&mut out, "stale_info:".to_owned(), false);
        if !info.reason.is_empty() {
            push(&mut out, format!("  reason: {}", info.reason), true);
        }
        if !info.recommended_action.is_empty() {
            push(
                &mut out,
                format!("  action: {}", info.recommended_action),
                true,
            );
        }
        if info.pending_messages > 0 {
            push(
                &mut out,
                format!("  pending_messages: {}", info.pending_messages),
                true,
            );
        }
        if info.wake_pending {
            push(
                &mut out,
                format!("  wake_attempts: {}", info.wake_attempts),
                true,
            );
        }
        if info.state_divergence {
            push(
                &mut out,
                format!("  divergence: {}", info.state_divergence_note),
                true,
            );
        }
    }

    let logs: Vec<_> = task.logs.iter().rev().take(3).collect();
    if !logs.is_empty() {
        out.push(Line::from(""));
        push(&mut out, "recent logs:".to_owned(), false);
        for log in logs.into_iter().rev() {
            push(
                &mut out,
                format!(
                    "  {} {}: {}",
                    log.timestamp.format("%H:%M:%S"),
                    log.workspace,
                    log.message
                ),
                true,
            );
        }
    }

    let activity = crate::tasks::build_task_activity(task, history, 4);
    if !activity.is_empty() {
        out.push(Line::from(""));
        push(&mut out, "activity:".to_owned(), false);
        for entry in &activity {
            push(
                &mut out,
                format!(
                    "  {} {:<9} {}",
                    entry.timestamp.format("%H:%M:%S"),
                    entry.kind.label(),
                    entry.summary,
                ),
                true,
            );
        }
    }

    if out.len() > height {
        out.truncate(height);
    }
    out
}

fn draw_selection_summary(f: &mut Frame, area: Rect, app: &App) {
    // Draw the tmux capture grid when there are live sessions to
    // preview; fall back to the selection summary when the daemon
    // is idle.
    if !app.sessions.is_empty() {
        draw_capture_grid(f, area, app);
        return;
    }
    let body = match current_entry(app) {
        Some(entry) => {
            let ws = &entry.workspace;
            let info = app
                .workspace_infos
                .get(ws)
                .map_or_else(|| "(offline)".to_owned(), workspace_info_summary);
            format!(
                "workspace: {ws}\nsession: {}\nagent:    {}",
                entry
                    .session_index
                    .and_then(|idx| app.sessions.get(idx))
                    .map_or_else(|| "none".to_owned(), |s| s.name.clone()),
                info,
            )
        }
        None => "pick a workspace on the left".to_owned(),
    };
    let para = Paragraph::new(body).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(app.stream.title()),
    );
    f.render_widget(para, area);
}

const CAPTURE_CARD_WIDTH: u16 = 36;
const CAPTURE_CARD_HEIGHT: u16 = 8;

fn draw_capture_grid(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(StreamView::Hidden.title());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let columns = (inner.width / CAPTURE_CARD_WIDTH.max(1)).max(1) as usize;
    let card_w = CAPTURE_CARD_WIDTH.min(inner.width);
    let card_h = CAPTURE_CARD_HEIGHT.min(inner.height);
    let rows = (inner.height / card_h.max(1)).max(1) as usize;
    let budget = columns.saturating_mul(rows);

    // Show every workspace that's running a tmux session, cut at
    // the grid capacity; extras simply don't render (operator can
    // enlarge the window or pick another view).
    let focused = app.selected_workspace();
    for (idx, session) in app.sessions.iter().take(budget).enumerate() {
        let col = (idx % columns) as u16;
        let row = (idx / columns) as u16;
        let x = inner.x + col * card_w;
        let y = inner.y + row * card_h;
        let card_area = Rect::new(x, y, card_w, card_h);
        draw_capture_card(f, card_area, session, app, focused);
    }
}

fn draw_capture_card(
    f: &mut Frame,
    area: Rect,
    session: &ax_tmux::SessionInfo,
    app: &App,
    focused: Option<&str>,
) {
    let selected = focused == Some(session.workspace.as_str());
    let border_style = if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = format!(" {} ", session.workspace);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let capture = app
        .captures
        .entries
        .get(&session.workspace)
        .map_or("", |entry| entry.content.as_str());
    if capture.is_empty() {
        let para =
            Paragraph::new("(capturing…)").style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, inner);
        return;
    }
    let rows = inner.height as usize;
    let width = inner.width as usize;
    let lines: Vec<Line> = crate::captures::recent_lines(capture, rows)
        .into_iter()
        .map(|line| Line::from(Span::raw(sanitize_capture_line(line, width))))
        .collect();
    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

/// Strip ANSI escape sequences + truncate to the given width so
/// capture previews don't break the grid border. We capture
/// without `-e` (no escapes) so this is just a width clamp today;
/// keeping the hook for when colour passthrough lands.
fn sanitize_capture_line(line: &str, width: usize) -> String {
    let mut clean: String = line
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\t'))
        .collect();
    // Replace tabs with spaces so we keep single-row alignment.
    clean = clean.replace('\t', "  ");
    if clean.chars().count() <= width {
        return clean;
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_owned();
    }
    let mut truncated: String = clean.chars().take(width - 1).collect();
    truncated.push('…');
    truncated
}

fn current_entry(app: &App) -> Option<&SidebarEntry> {
    app.sidebar_entries
        .get(app.selected_entry)
        .filter(|e| !e.group && e.session_index.is_some())
}

fn workspace_info_summary(info: &ax_proto::types::WorkspaceInfo) -> String {
    let status = agent_status_str(&info.status);
    if info.status_text.is_empty() {
        status.to_owned()
    } else {
        format!("{status} — {}", info.status_text)
    }
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let (text, style) = if let Some(notice) = &app.quick_notice {
        let s = if notice.error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        (notice.text.clone(), s)
    } else if let Some(msg) = &app.notice {
        (msg.clone(), Style::default().add_modifier(Modifier::DIM))
    } else if app.quick_actions.open {
        (
            "↑↓ action · enter run · esc close · q quit".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        )
    } else {
        (
            "j/k sidebar · [/] tasks · f filter · Tab/s view · esc actions · q quit".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        )
    };
    let footer = Paragraph::new(text).style(style);
    f.render_widget(footer, area);
}

fn agent_status_str(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::Online => "online",
        AgentStatus::Offline => "offline",
        AgentStatus::Disconnected => "disconnected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_range_handles_empty_and_small_windows() {
        assert_eq!(viewport_range(0, 0, 5), (0, 0));
        assert_eq!(viewport_range(10, 3, 0), (0, 0));
    }

    #[test]
    fn viewport_range_scrolls_so_selected_stays_in_view() {
        assert_eq!(viewport_range(20, 0, 5), (0, 5));
        assert_eq!(viewport_range(20, 4, 5), (0, 5));
        assert_eq!(viewport_range(20, 5, 5), (1, 6));
        assert_eq!(viewport_range(20, 19, 5), (15, 20));
    }
}
