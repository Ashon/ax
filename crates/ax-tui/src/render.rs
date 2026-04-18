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

pub(crate) fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(1),    // agents + tabs + content
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(f, chunks[0], app);

    let streaming = app.streamed_workspace.is_some();
    let agents_h = compute_agents_height(app, chunks[1].height, streaming);
    let middle = if streaming {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(agents_h), Constraint::Min(1)])
            .split(chunks[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(agents_h),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(chunks[1])
    };
    let agents_area = middle[0];
    let content_area = *middle.last().expect("layout produces >= 2 rows");
    draw_sidebar(f, agents_area, app);
    if !streaming {
        draw_stream_tabs(f, middle[1], app);
    }
    draw_body(f, content_area, app);

    if app.quick_actions.open {
        draw_quick_actions(f, area, agents_area, app);
    }

    draw_footer(f, chunks[2], app);
}

/// Clamp the agents pane so it shows every row when possible but
/// never starves the tab+content pane below it. Overflow rows scroll
/// within the pane; `+3` accounts for the border (2) and header (1).
fn compute_agents_height(app: &App, middle_h: u16, streaming: bool) -> u16 {
    let reserved = if streaming { 3 } else { 4 };
    let desired = (app.sidebar_entries.len() as u16).saturating_add(3).max(5);
    let cap = middle_h.saturating_sub(reserved).max(3);
    desired.min(cap)
}

/// Context-menu style overlay: drops down from the selected agent
/// row with a small indent so it reads as a popup on that row.
/// Clamps into the frame so it never runs off the edge.
fn draw_quick_actions(f: &mut Frame, frame: Rect, sidebar: Rect, app: &App) {
    let workspace = app.selected_workspace().unwrap_or("");
    let action_count = app.quick_actions.actions.len() as u16;
    let height = (action_count + 4).min(frame.height.saturating_sub(2)).max(4);
    let width: u16 = 42;
    let width = width.min(frame.width.saturating_sub(2));

    let selected_row = sidebar.y + 1 + app.selected_entry as u16;
    let anchor_x = sidebar.x + 2;
    let anchor_y = selected_row + 1;
    let max_x = frame.right().saturating_sub(width);
    let max_y = frame.bottom().saturating_sub(height);
    let x = anchor_x.min(max_x).max(frame.x);
    let y = anchor_y.min(max_y).max(frame.y);
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
    let block = Block::default().borders(Borders::ALL).title(" agents ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if app.sidebar_entries.is_empty() {
        let empty = Paragraph::new(
            "No active agents. Run `ax up` in a project directory with .ax/config.yaml.",
        )
        .wrap(Wrap { trim: true });
        f.render_widget(empty, inner);
        return;
    }

    let cols = SidebarColumns::fit(inner.width);
    let header_area = Rect::new(inner.x, inner.y, inner.width, 1);
    draw_sidebar_header(f, header_area, &cols);

    let list_area = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    // Slice the entry list so the selection stays inside the visible
    // rows; overflow above/below scrolls as the cursor moves.
    let (start, end) = viewport_range(
        app.sidebar_entries.len(),
        app.selected_entry,
        list_area.height as usize,
    );
    let items: Vec<ListItem> = app.sidebar_entries[start..end]
        .iter()
        .enumerate()
        .map(|(rel, entry)| sidebar_row(start + rel, entry, app, &cols))
        .collect();
    f.render_widget(List::new(items), list_area);

    draw_sidebar_scroll_hints(f, list_area, start, end, app.sidebar_entries.len());
}

/// Overlay "↑N more" / "↓N more" markers at the right edge of the
/// list when entries scroll off-screen so operators see there's
/// more to reach.
fn draw_sidebar_scroll_hints(f: &mut Frame, list_area: Rect, start: usize, end: usize, total: usize) {
    if list_area.height == 0 || list_area.width < 10 {
        return;
    }
    if start > 0 {
        let hint = format!("↑{start}");
        let width = hint.chars().count() as u16;
        let x = list_area.right().saturating_sub(width);
        let rect = Rect::new(x, list_area.y, width, 1);
        let para = Paragraph::new(hint).style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, rect);
    }
    let remaining = total.saturating_sub(end);
    if remaining > 0 {
        let hint = format!("↓{remaining}");
        let width = hint.chars().count() as u16;
        let x = list_area.right().saturating_sub(width);
        let y = list_area.y + list_area.height.saturating_sub(1);
        let rect = Rect::new(x, y, width, 1);
        let para = Paragraph::new(hint).style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, rect);
    }
}

/// Column widths for the agents table. `name` flexes to fill the
/// remainder; the rest are fixed so rows line up across refreshes.
struct SidebarColumns {
    name: usize,
    state: usize,
    up: usize,
    down: usize,
    cost: usize,
    info: usize,
}

impl SidebarColumns {
    fn fit(width: u16) -> Self {
        let state = 12;
        let up = 8;
        let down = 8;
        let cost = 10;
        let info = 14;
        let gaps = 5; // 5 single-space gaps between 6 columns
        let fixed = state + up + down + cost + info + gaps;
        let name = (width as usize).saturating_sub(fixed).max(10);
        Self {
            name,
            state,
            up,
            down,
            cost,
            info,
        }
    }
}

fn draw_sidebar_header(f: &mut Frame, area: Rect, cols: &SidebarColumns) {
    let text = format!(
        "{:<w1$} {:<w2$} {:<w3$} {:<w4$} {:<w5$} {:<w6$}",
        "NAME",
        "STATE",
        "UP",
        "DOWN",
        "COST",
        "INFO",
        w1 = cols.name,
        w2 = cols.state,
        w3 = cols.up,
        w4 = cols.down,
        w5 = cols.cost,
        w6 = cols.info,
    );
    let para = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().add_modifier(Modifier::DIM),
    )));
    f.render_widget(para, area);
}

fn sidebar_row<'a>(
    idx: usize,
    entry: &'a SidebarEntry,
    app: &'a App,
    cols: &SidebarColumns,
) -> ListItem<'a> {
    if entry.group {
        let indent = "  ".repeat(entry.level);
        return ListItem::new(Line::from(Span::styled(
            format!("{indent}{}", entry.label),
            Style::default().add_modifier(Modifier::BOLD),
        )));
    }

    let live = entry.session_index.is_some();
    let is_selected = idx == app.selected_entry;
    let cursor = if is_selected { "▸" } else { " " };
    let indent = "  ".repeat(entry.level);
    let marker = if live { "●" } else { "○" };
    let name_raw = format!("{cursor} {indent}{marker} {}", entry.label);

    let info_opt = app.workspace_infos.get(&entry.workspace);
    let state_raw = info_opt.map_or("offline", |w| agent_status_str(&w.status));

    let capture = app
        .captures
        .entries
        .get(&entry.workspace)
        .map_or("", |e| e.content.as_str());
    let tokens = crate::tokens::parse_agent_tokens(&entry.workspace, capture);
    let up_raw = token_cell(&tokens.up, '↑');
    let down_raw = token_cell(&tokens.down, '↓');
    let cost_raw = if tokens.cost.is_empty() {
        "-".to_owned()
    } else {
        tokens.cost
    };

    let info_raw = if entry.reconcile.is_empty() {
        info_opt
            .map(|w| w.status_text.clone())
            .unwrap_or_default()
    } else {
        entry.reconcile.clone()
    };

    let text = format!(
        "{name} {state} {up} {down} {cost} {info}",
        name = pad_or_trunc(&name_raw, cols.name),
        state = pad_or_trunc(state_raw, cols.state),
        up = pad_or_trunc(&up_raw, cols.up),
        down = pad_or_trunc(&down_raw, cols.down),
        cost = pad_or_trunc(&cost_raw, cols.cost),
        info = pad_or_trunc(&info_raw, cols.info),
    );

    let style = if is_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else if !live {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    ListItem::new(Line::from(Span::styled(text, style)))
}

fn token_cell(raw: &str, arrow: char) -> String {
    if raw.is_empty() {
        return "-".to_owned();
    }
    let value = crate::tokens::parse_token_value(raw);
    format!("{arrow}{}", crate::tokens::format_token_count(value))
}

fn pad_or_trunc(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count > width {
        return crate::tasks::truncate(s, width);
    }
    let mut out = s.to_owned();
    for _ in count..width {
        out.push(' ');
    }
    out
}

fn draw_body(f: &mut Frame, area: Rect, app: &App) {
    if let Some(workspace) = app.streamed_workspace.clone() {
        draw_stream_single(f, area, app, &workspace);
        return;
    }
    match app.stream {
        StreamView::Messages => draw_messages(f, area, app),
        StreamView::Tasks => draw_tasks(f, area, app),
        StreamView::Tokens => draw_tokens(f, area, app),
    }
}

/// Tab strip above the stream pane. Tab/s and the number keys 1-3
/// drive the switch.
fn draw_stream_tabs(f: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mut spans: Vec<Span> = Vec::with_capacity(StreamView::ALL.len() * 2);
    for (idx, view) in StreamView::ALL.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(
                " │ ",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        let label = format!(" {}·{} ", idx + 1, view.tab_label());
        let style = if *view == app.stream {
            Style::default()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        spans.push(Span::styled(label, style));
    }
    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}

fn draw_stream_single(f: &mut Frame, area: Rect, app: &App, workspace: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" {workspace} · tmux stream · esc to exit "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let capture = app
        .captures
        .entries
        .get(workspace)
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

fn draw_tokens(f: &mut Frame, area: Rect, app: &App) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(StreamView::Tokens.title());

    // Scan every active session's tmux capture for token markers,
    // keeping rows ordered the same way the sidebar groups them so
    // the eye can trace sidebar → token row easily.
    let mut rows: Vec<crate::tokens::AgentTokens> = app
        .sessions
        .iter()
        .map(|s| {
            let capture = app
                .captures
                .entries
                .get(&s.workspace)
                .map_or("", |entry| entry.content.as_str());
            crate::tokens::parse_agent_tokens(&s.workspace, capture)
        })
        .filter(|t| !t.is_empty())
        .collect();
    rows.sort_by(|a, b| a.workspace.cmp(&b.workspace));

    if rows.is_empty() {
        let hint = if app.sessions.is_empty() {
            "  (no active agents — run `ax up` to start workspaces)"
        } else {
            "  (no token markers in recent tmux captures)"
        };
        let para = Paragraph::new(hint)
            .style(Style::default().add_modifier(Modifier::DIM))
            .block(block);
        f.render_widget(para, area);
        return;
    }

    let max_cost = rows
        .iter()
        .map(|r| crate::tokens::parse_cost_value(&r.cost))
        .fold(0.0_f64, f64::max);

    let mut lines: Vec<Line> = Vec::with_capacity(inner_height);
    lines.push(Line::from(Span::styled(
        " live usage ",
        Style::default().add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(Span::styled(
        " WORKSPACE              INPUT      OUTPUT      COST",
        Style::default().add_modifier(Modifier::DIM),
    )));

    let budget = inner_height.saturating_sub(lines.len());
    for tokens in rows.into_iter().take(budget) {
        let up_display = if tokens.up.is_empty() {
            "-".to_owned()
        } else {
            format!(
                "↑{}",
                crate::tokens::format_token_count(crate::tokens::parse_token_value(&tokens.up))
            )
        };
        let down_display = if tokens.down.is_empty() {
            "-".to_owned()
        } else {
            format!(
                "↓{}",
                crate::tokens::format_token_count(crate::tokens::parse_token_value(&tokens.down))
            )
        };
        let cost = crate::tokens::parse_cost_value(&tokens.cost);
        let cost_display = if tokens.cost.is_empty() {
            "-".to_owned()
        } else {
            tokens.cost.clone()
        };
        let style = if max_cost > 0.0 && cost >= max_cost * 0.8 {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };
        let row = format!(
            " {:<22} {:<10} {:<10} {}",
            crate::tasks::truncate(&tokens.workspace, 22),
            up_display,
            down_display,
            cost_display,
        );
        lines.push(Line::from(Span::styled(
            crate::tasks::truncate(&row, inner_width.max(1)),
            style,
        )));
    }
    let para = Paragraph::new(lines).block(block);
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

/// Strip control characters + truncate to the given width so the
/// single-workspace stream mirror doesn't break its border.
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
    } else if app.streamed_workspace.is_some() {
        (
            "esc exit stream · q quit".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        )
    } else {
        (
            "j/k sidebar · 1-4/Tab view · [/] tasks · f filter · enter actions · q quit"
                .to_owned(),
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
