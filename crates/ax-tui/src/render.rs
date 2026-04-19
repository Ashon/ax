//! ratatui draw routine. Body is a tab strip over a vertical
//! list/detail split: every tab — agents, messages, tasks, tokens,
//! stream — owns both a list renderer (top) and a detail renderer
//! (bottom). No standalone agents pane anymore; fleet visibility
//! moves to the header status bar when the active tab isn't agents.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Sparkline, Wrap,
};
use ratatui::Frame;
use throbber_widgets_tui::{Throbber, WhichUse, BRAILLE_SIX};

use ax_proto::types::AgentStatus;

use crate::actions::QuickActionId;
use crate::agents::AgentEntry;
use crate::state::{App, Focus};
use crate::stream::{format_message_line, StreamView};

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header / status bar
            Constraint::Min(1),    // body = tab strip + list + detail
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(f, chunks[0], app);
    let body_area = chunks[1];
    draw_body(f, body_area, app);

    if app.quick_actions.open {
        // Overlay anchors to the list pane since that's where the
        // agents cursor lives when it's open.
        let (list_area, _) = split_body_inner(body_area);
        draw_quick_actions(f, area, list_area, app);
    }
    if app.help_open {
        draw_help(f, area);
    }

    draw_footer(f, chunks[2], app);
}

/// Split the body's inner rect into `(list, detail)` vertical halves.
/// Returns the `list` rect first so the selected-row math anchors on
/// the top pane. Gives list 45% and detail 55% by default — detail
/// wins the extra row so long task logs / message content stay
/// legible without scrolling. `compute_body_inner` wraps the border
/// math so overlays can recompute the same layout without repeating
/// the outer block.
fn split_body_inner(body_area: Rect) -> (Rect, Rect) {
    // Mirror `draw_body`'s outer block: one row of top border (tabs
    // title), two rows consumed by side + bottom borders, no extra
    // padding. Detail pane is rendered directly under the list.
    let inner = Rect::new(
        body_area.x + 1,
        body_area.y + 1,
        body_area.width.saturating_sub(2),
        body_area.height.saturating_sub(2),
    );
    let list_h = (inner.height * 45 / 100).max(3).min(inner.height.saturating_sub(3));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(list_h), Constraint::Min(3)])
        .split(inner);
    (chunks[0], chunks[1])
}

/// Centered keybinding cheatsheet. Opened via `?` from any non-overlay
/// context, dismissed via `?` or `Esc`. Reuses the `Clear` + framed
/// block pattern from `draw_quick_actions` so the visual language
/// stays consistent.
fn draw_help(f: &mut Frame, frame: Rect) {
    const SECTIONS: &[(&str, &[(&str, &str)])] = &[
        (
            "global",
            &[
                ("?", "toggle this help"),
                ("q / ctrl-c", "quit"),
                ("[ / ]", "switch pane (list ↔ detail)"),
                ("Tab / Shift-Tab", "cycle tab"),
                ("1-5", "agents · messages · tasks · tokens · stream"),
                ("f", "cycle task filter"),
            ],
        ),
        (
            "list · agents",
            &[
                ("↑ ↓ / j k", "move agent cursor"),
                ("Enter", "open action menu"),
                ("wheel", "scroll list"),
            ],
        ),
        (
            "list · tasks",
            &[
                ("↑ ↓ / j k", "move selected task"),
                ("wheel", "scroll list"),
            ],
        ),
        (
            "list · messages / tokens",
            &[
                ("↑ ↓ / j k", "scroll"),
                ("PgUp / PgDn", "scroll by page"),
                ("g / G", "head / tail"),
                ("wheel", "scroll"),
            ],
        ),
        (
            "detail",
            &[
                ("↑ ↓ / j k", "scroll detail"),
                ("PgUp / PgDn", "scroll by page"),
                ("g", "top"),
                ("Esc", "back to list"),
            ],
        ),
        (
            "action menu",
            &[
                ("↑ ↓", "select action"),
                ("Enter", "run (re-press to confirm destructive ops)"),
                ("Esc", "close"),
            ],
        ),
    ];

    let total_rows: u16 = SECTIONS
        .iter()
        .map(|(_, rows)| rows.len() as u16 + 1)
        .sum();
    let height = (total_rows + 2)
        .min(frame.height.saturating_sub(2))
        .max(6);
    let width: u16 = 56;
    let width = width.min(frame.width.saturating_sub(2));

    let x = frame.x + frame.width.saturating_sub(width) / 2;
    let y = frame.y + frame.height.saturating_sub(height) / 2;
    let area = Rect::new(x, y, width, height);

    f.render_widget(ratatui::widgets::Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" help · ? or esc to close ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let key_col = 18usize;
    let mut lines: Vec<Line> = Vec::with_capacity(total_rows as usize);
    for (idx, (section, rows)) in SECTIONS.iter().enumerate() {
        if idx > 0 {
            // Blank separator between sections so the cheatsheet reads
            // as a vertically-stacked set of groups rather than one
            // long table.
            lines.push(Line::from(Span::raw("")));
        }
        lines.push(Line::from(Span::styled(
            format!(" {section}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for (key, desc) in *rows {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {key:<width$}", width = key_col),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(*desc),
            ]));
        }
    }
    let visible: Vec<Line> = lines.into_iter().take(inner.height as usize).collect();
    f.render_widget(Paragraph::new(visible), inner);
}

/// Context-menu style overlay: drops down from the selected agent
/// row with a small indent so it reads as a popup on that row.
/// Clamps into the frame so it never runs off the edge.
///
/// `list_area` is the list half of the body (already inside the
/// outer block). The agents list reserves 1 row for its column
/// header, then paints rows from `list_area.y + 1` down.
fn draw_quick_actions(f: &mut Frame, frame: Rect, list_area: Rect, app: &App) {
    let workspace = app.selected_workspace().unwrap_or("");
    let action_count = app.quick_actions.actions.len() as u16;
    // Content rows: confirm mode is a two-line prompt, normal mode
    // is one line per action plus a footer hint. Borders contribute
    // two rows.
    let content_rows = if app.quick_actions.confirm {
        2
    } else {
        action_count + 1
    };
    let height = (content_rows + 2)
        .min(frame.height.saturating_sub(2))
        .max(4);
    let width: u16 = 42;
    let width = width.min(frame.width.saturating_sub(2));

    // Reproduce the agents-panel layout so the popup appears *below*
    // the row under the cursor (even when the list has scrolled off
    // the top). The list pane reserves 1 row for its column header
    // before the agent rows start.
    let rows_height = list_area.height.saturating_sub(1);
    let viewport = compute_viewport(
        app.agent_entries.len(),
        app.selected_entry,
        rows_height as usize,
    );
    let rows_y = list_area.y.saturating_add(1);
    let rel = (app.selected_entry.saturating_sub(viewport.start)) as u16;
    let selected_row = rows_y.saturating_add(rel);

    let anchor_x = list_area.x + 2;
    let below = selected_row.saturating_add(1);
    let max_x = frame.right().saturating_sub(width);
    let max_y = frame.bottom().saturating_sub(height);
    // Prefer a position directly under the selected row. If there's
    // not enough space below, flip the popup to sit above the row so
    // the selection stays visible instead of being hidden by the
    // overlay clamping back up.
    let y = if below <= max_y {
        below.max(frame.y)
    } else if selected_row > frame.y.saturating_add(height) {
        selected_row.saturating_sub(height)
    } else {
        max_y.max(frame.y)
    };
    let x = anchor_x.min(max_x).max(frame.x);
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
    let online = app
        .workspace_infos
        .values()
        .filter(|w| matches!(w.status, AgentStatus::Online))
        .count();
    let total_agents = app.workspace_infos.len();
    let active_tasks = app
        .tasks
        .iter()
        .filter(|t| {
            matches!(
                t.status,
                ax_proto::types::TaskStatus::Pending | ax_proto::types::TaskStatus::InProgress
            )
        })
        .count();
    // Collapse segments so the line stays under a single terminal row
    // even on narrow windows. `agents: 0/0` and an empty task set are
    // common on a freshly-booted repo, so keep them visible as a cue
    // that the surface is wired up.
    let text = format!(
        "ax · daemon: {daemon} · agents: {online}/{total_agents} · tasks: {active_tasks} active / {total_tasks} · sessions: {sessions} · filter: {filter}",
        total_tasks = app.tasks.len(),
        sessions = app.sessions.len(),
        filter = app.task_filter.label(),
    );
    if app.daemon_running {
        let throbber = status_throbber(text, Style::default().add_modifier(Modifier::BOLD));
        let mut state = app.throbber_state.clone();
        f.render_stateful_widget(throbber, area, &mut state);
    } else {
        let header = Paragraph::new(text).style(Style::default().add_modifier(Modifier::BOLD));
        f.render_widget(header, area);
    }
}

/// Agents list pane. Renders directly into the body's list area (no
/// outer block — the tab strip's block owns that). Shows the column
/// header + agent rows with a scrollbar when entries overflow.
fn draw_agents_list(f: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if app.agent_entries.is_empty() {
        let empty = Paragraph::new(
            "No active agents. Run `ax up` in a project directory with .ax/config.yaml.",
        )
        .wrap(Wrap { trim: true });
        f.render_widget(empty, area);
        return;
    }

    let cols = AgentColumns::fit(area.width);
    let header_area = Rect::new(area.x, area.y, area.width, 1);
    draw_agents_header(f, header_area, &cols);

    let list_area = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    if list_area.height == 0 {
        return;
    }
    let viewport = compute_viewport(
        app.agent_entries.len(),
        app.selected_entry,
        list_area.height as usize,
    );
    let rows_area = viewport.content_area(list_area);
    let items: Vec<ListItem> = app.agent_entries[viewport.start..viewport.end]
        .iter()
        .enumerate()
        .map(|(rel, entry)| agent_row(viewport.start + rel, entry, app, &cols))
        .collect();
    f.render_widget(List::new(items), rows_area);
    render_scrollbar(f, list_area, viewport);
}

/// Column widths for the agents table. `name` flexes to fill the
/// remainder; the rest are fixed so rows line up across refreshes.
struct AgentColumns {
    name: usize,
    state: usize,
    up: usize,
    down: usize,
    cost: usize,
    info: usize,
}

impl AgentColumns {
    fn fit(width: u16) -> Self {
        // NAME is fixed-compact; INFO absorbs the remaining width so
        // operator hints / reconcile notes get the room they need.
        let name = 28;
        let state = 11;
        let up = 7;
        let down = 7;
        let cost = 8;
        let gaps = 5; // 5 single-space gaps between 6 columns
        let fixed = name + state + up + down + cost + gaps;
        let info = (width as usize).saturating_sub(fixed).max(12);
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

fn draw_agents_header(f: &mut Frame, area: Rect, cols: &AgentColumns) {
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

fn agent_row<'a>(
    idx: usize,
    entry: &'a AgentEntry,
    app: &'a App,
    cols: &AgentColumns,
) -> ListItem<'a> {
    // Two-column indent per depth level, starting from zero at the
    // top-level project so the tree reads "L1 → 0, L2 → 2, …". Leaves
    // additionally carry a one-char cursor column, which visually
    // shifts them right of their parent group — that's intentional so
    // the `▸` cursor has somewhere to land without pushing the label.
    let indent = "  ".repeat(entry.level);
    if entry.group {
        return ListItem::new(Line::from(Span::styled(
            format!("{indent}{}", entry.label),
            Style::default().add_modifier(Modifier::BOLD),
        )));
    }

    let live = entry.session_index.is_some();
    let is_selected = idx == app.selected_entry;
    let cursor = if is_selected { "▸" } else { " " };
    let marker = if live { "●" } else { "○" };
    let name_raw = format!("{cursor} {indent}{marker} {}", entry.label);

    let info_opt = app.workspace_infos.get(&entry.workspace);
    // Flip between "running" (animated spinner) and "idle" based on
    // whether the tmux capture is actively changing. A few seconds of
    // quiet output past the last captured diff means the agent is
    // waiting for input, so the spinner stops. Offline sessions show
    // the plain daemon-reported status.
    let state_raw: String = if live {
        if app
            .captures
            .is_recently_active(&entry.workspace, std::time::Instant::now())
        {
            format!("{} running", spinner_frame(&app.throbber_state))
        } else {
            "idle".to_owned()
        }
    } else {
        info_opt
            .map_or("offline", |w| agent_status_str(&w.status))
            .to_owned()
    };

    // Prefer the live tmux-capture numbers when a Claude session is
    // actively rendering its `↑ tokens ↓ tokens · $x.yz` footer —
    // that gives us a current-turn readout. Otherwise fall back to
    // the daemon's persisted `usage_trends` totals so offline agents
    // still show the tokens they logged before going idle.
    let capture = app
        .captures
        .entries
        .get(&entry.workspace)
        .map_or("", |e| e.content.as_str());
    let live_tokens = crate::tokens::parse_agent_tokens(&entry.workspace, capture);
    let trend = app.usage_trends.get(&entry.workspace);
    let (up_raw, down_raw, cost_raw) = if !live_tokens.is_empty() {
        let cost = if live_tokens.cost.is_empty() {
            "-".to_owned()
        } else {
            live_tokens.cost.clone()
        };
        (
            token_cell(&live_tokens.up, '↑'),
            token_cell(&live_tokens.down, '↓'),
            cost,
        )
    } else if let Some(t) = trend.filter(|t| t.available && t.total.total() > 0) {
        // Persisted trend has no cost tracking (that's a Claude CLI
        // footer artifact), so we substitute the cumulative total as
        // the third number instead of leaving the column blank.
        let total_all = (t.total.cache_read + t.total.cache_creation + t.total.input) as f64;
        (
            format!("↑{}", crate::tokens::format_token_count(t.total.input as f64)),
            format!(
                "↓{}",
                crate::tokens::format_token_count(t.total.output as f64)
            ),
            format!("Σ{}", crate::tokens::format_token_count(total_all)),
        )
    } else {
        ("-".to_owned(), "-".to_owned(), "-".to_owned())
    };

    let info_raw = if entry.reconcile.is_empty() {
        info_opt.map(|w| w.status_text.clone()).unwrap_or_default()
    } else {
        entry.reconcile.clone()
    };

    let text = format!(
        "{name} {state} {up} {down} {cost} {info}",
        name = pad_or_trunc(&name_raw, cols.name),
        state = pad_or_trunc(&state_raw, cols.state),
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

fn draw_body(f: &mut Frame, area: Rect, app: &mut App) {
    // Outer body block carries the tab strip in its top border so the
    // layout doesn't burn a row on a standalone tab row. The inner
    // area splits vertically into a list pane (top) and a detail
    // pane (bottom); each tab owns a renderer for both halves.
    let block = Block::default()
        .borders(Borders::ALL)
        // Border stays neutral — focus is communicated inline on the
        // list/detail pane titles so the outer frame doesn't strobe
        // between cyan and default as the user toggles `[/]`.
        .border_style(Style::default())
        .title(tabs_title(app));
    f.render_widget(block, area);
    let (list_area, detail_area) = split_body_inner(area);
    if list_area.width == 0 || list_area.height == 0 {
        return;
    }

    draw_list_pane(f, list_area, app);
    // A subtle top border on the detail pane doubles as the divider
    // between the two halves. Focus-cyan highlights whichever side
    // currently owns the keyboard.
    draw_detail_pane(f, detail_area, app);
}

/// Dispatch the list half of the body to the per-tab renderer.
fn draw_list_pane(f: &mut Frame, area: Rect, app: &mut App) {
    match app.stream {
        StreamView::Agents => draw_agents_list(f, area, app),
        StreamView::Messages => draw_messages(f, area, app),
        StreamView::Tasks => draw_tasks_list_only(f, area, app),
        StreamView::Tokens => draw_tokens(f, area, app),
        StreamView::Stream => draw_stream(f, area, app),
    }
}

/// Dispatch the detail half of the body to the per-tab renderer.
/// Outer divider + focus coloring live here so every tab inherits
/// the same visual frame without duplicating border logic.
fn draw_detail_pane(f: &mut Frame, area: Rect, app: &mut App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let title = detail_title(app);
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(focus_border_style(app, Focus::Detail))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    match app.stream {
        StreamView::Agents => draw_agents_detail(f, inner, app),
        StreamView::Messages => draw_messages_detail(f, inner, app),
        StreamView::Tasks => draw_task_detail_only(f, inner, app),
        StreamView::Tokens => draw_tokens_detail(f, inner, app),
        StreamView::Stream => draw_stream_detail(f, inner, app),
    }
}

/// Compose the detail block's top-border title. Surfaces the selected
/// row identifier so operators can see what the detail is describing
/// even after scrolling the detail body.
fn detail_title(app: &App) -> Line<'static> {
    let focus_detail = app.focus == Focus::Detail;
    let label = match app.stream {
        StreamView::Agents => app
            .selected_workspace()
            .map(|w| format!(" detail · {w} "))
            .unwrap_or_else(|| " detail ".to_owned()),
        StreamView::Messages => " message detail ".to_owned(),
        StreamView::Tasks => " task detail ".to_owned(),
        StreamView::Tokens => " token detail ".to_owned(),
        StreamView::Stream => app
            .streamed_workspace
            .as_deref()
            .map(|w| format!(" stream · {w} "))
            .unwrap_or_else(|| " stream detail ".to_owned()),
    };
    let style = if focus_detail {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    Line::from(Span::styled(label, style))
}

/// Build the body block's title as a tab strip. The line sits on the
/// top border of whichever body sub-view is active, so we don't burn
/// an extra row on a standalone tab strip above the pane. The tabs
/// belong to the Body panel now (no standalone Tabs focus), so the
/// highlighted tab turns cyan bold whenever Body is focused and
/// falls back to white bold with dim siblings otherwise.
fn tabs_title(app: &App) -> Line<'static> {
    let focused = app.focus == Focus::Detail;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (idx, view) in StreamView::ALL.iter().enumerate() {
        if idx > 0 {
            // Use the same horizontal glyph as the surrounding block
            // border so the divider melts into the top edge instead
            // of poking up as a dot.
            spans.push(Span::styled(
                format!(" {} ", symbols::line::HORIZONTAL),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        let label = format!(" {}·{} ", idx + 1, view.tab_label());
        let is_selected = *view == app.stream;
        let style = match (is_selected, focused) {
            (true, true) => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            (true, false) => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            (false, true) => Style::default().fg(Color::DarkGray),
            (false, false) => Style::default().add_modifier(Modifier::DIM),
        };
        spans.push(Span::styled(label, style));
    }
    Line::from(spans)
}

/// Render the live tmux capture of `app.streamed_workspace` into the
/// body's inner rect. The outer block + tab strip are already painted
/// by `draw_body`, so this routine only draws the capture lines (or a
/// placeholder when no workspace has been picked yet).
fn draw_stream(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(workspace) = app.streamed_workspace.clone() else {
        let placeholder = Paragraph::new(
            "  (no workspace streaming — focus an agent and hit enter → Stream tmux)",
        )
        .style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(placeholder, area);
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }

    let capture = app
        .captures
        .entries
        .get(&workspace)
        .map_or("", |entry| entry.content.as_str());
    if capture.is_empty() {
        let loading_area = centered_loading_area(area);
        let throbber = status_throbber(
            format!("waiting for tmux capture of {workspace}…"),
            Style::default().add_modifier(Modifier::DIM),
        );
        f.render_stateful_widget(throbber, loading_area, &mut app.throbber_state);
        return;
    }

    // Give the header a single-row caption so the streaming target
    // stays identifiable even though the tab strip only says
    // "stream". Mirrors the old hijack-mode title.
    let caption = Paragraph::new(format!("  {workspace} · tmux mirror"))
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    let caption_area = Rect::new(area.x, area.y, area.width, 1);
    f.render_widget(caption, caption_area);
    let body_area = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
    if body_area.height == 0 {
        return;
    }

    let rows = body_area.height as usize;
    let width = body_area.width as usize;
    let lines: Vec<Line> = crate::captures::recent_wrapped_lines(capture, rows, width)
        .into_iter()
        .map(|line| Line::from(Span::raw(line)))
        .collect();
    let para = Paragraph::new(lines);
    f.render_widget(para, body_area);
}

/// Pick the current frame of the shared braille spinner based on the
/// app-wide [`ThrobberState`]. Each refresh tick advances the state
/// via `tick_animation`, so successive draws naturally cycle through
/// the symbols.
fn spinner_frame(state: &throbber_widgets_tui::ThrobberState) -> &'static str {
    let symbols = BRAILLE_SIX.symbols;
    let len = symbols.len() as i16;
    let idx = (i16::from(state.index())).rem_euclid(len) as usize;
    symbols[idx]
}

fn status_throbber<'a>(label: impl Into<Span<'a>>, style: Style) -> Throbber<'a> {
    Throbber::default()
        .label(label)
        .style(style)
        .throbber_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .throbber_set(BRAILLE_SIX)
        .use_type(WhichUse::Spin)
}

fn centered_loading_area(area: Rect) -> Rect {
    let width = area.width.saturating_sub(2).clamp(1, 36);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(1) / 2;
    Rect::new(x, y, width, 1)
}

/// Border style used by every panel block so the focused panel has
/// an obvious cyan edge and the rest fade to the default border
/// colour.
fn focus_border_style(app: &App, panel: Focus) -> Style {
    if app.focus == panel {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn draw_messages(f: &mut Frame, area: Rect, app: &App) {
    // `area` is the body block's inner rect — outer border + tab
    // strip are already painted by `draw_body`. Render raw content
    // into this area without stacking another block.
    let inner_height = area.height as usize;

    if app.messages.is_empty() {
        let para =
            Paragraph::new("  (no messages yet)").style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, area);
        return;
    }

    // Map `messages_cursor.index` (entries-from-tail) onto a viewport
    // window. The input handler never knows the pane height, so it
    // lets scroll over-shoot; clamping lives here.
    let total = app.messages.len();
    let visible = inner_height.max(1).min(total);
    let max_scroll = total.saturating_sub(visible);
    let scroll = app.messages_cursor.index.min(max_scroll);
    let end = total - scroll;
    let start = end.saturating_sub(visible);

    let viewport = Viewport {
        start,
        end,
        visible: end - start,
        total,
    };
    let content_area = viewport.content_area(area);
    let content_width = content_area.width as usize;
    let lines: Vec<Line> = app.messages[start..end]
        .iter()
        .map(|entry| Line::from(Span::raw(format_message_line(entry, content_width.max(1)))))
        .collect();
    f.render_widget(Paragraph::new(lines), content_area);
    render_scrollbar(f, area, viewport);
}

fn draw_tokens(f: &mut Frame, area: Rect, app: &App) {
    // `area` is the body block's inner rect — no additional border.
    let inner = area;

    // Pull rolled-up totals from the daemon's `usage_trends` cache
    // (populated by `app::refresh_usage`). Unlike the live tmux-
    // capture scraper this sees every workspace that has ever
    // produced a transcript, so offline agents still show their
    // historical token totals.
    let mut rows: Vec<&ax_proto::usage::WorkspaceTrend> = app
        .usage_trends
        .values()
        .filter(|trend| trend.available && trend.total.total() > 0)
        .collect();
    rows.sort_by(|a, b| a.workspace.cmp(&b.workspace));

    if rows.is_empty() {
        let hint = if !app.daemon_running {
            "  (daemon offline — start it with `ax up`)"
        } else if app.workspace_dirs.is_empty() {
            "  (no workspaces in config — add one to .ax/config.yaml)"
        } else if app.last_usage_refresh.is_none() {
            "  (collecting usage…)"
        } else {
            "  (no token usage recorded yet — run an agent to produce a transcript)"
        };
        f.render_widget(
            Paragraph::new(hint).style(Style::default().add_modifier(Modifier::DIM)),
            inner,
        );
        return;
    }

    let max_total = rows.iter().map(|t| t.total.total()).max().unwrap_or(0) as f64;

    // Caption + column header. One row each, rendered as plain
    // Paragraphs on the top of the inner area so the per-workspace
    // grid below can lean on a real Layout split (needed for the
    // inline Sparkline).
    let caption = Paragraph::new(" last 24h · ▁▂▃▄▅▆▇ = rolling usage per 5-min bucket")
        .style(Style::default().add_modifier(Modifier::DIM));
    let header = Paragraph::new(format!(
        " {:<24} {:<14} {:<9} {:<9} {:<9} {:<7} {:<12}  TREND",
        "WORKSPACE", "MODEL", "INPUT", "OUTPUT", "CACHE", "TURNS", "LAST"
    ))
    .style(Style::default().add_modifier(Modifier::DIM));

    let header_rows = 2_u16;
    if inner.height <= header_rows {
        f.render_widget(caption, inner);
        return;
    }
    let caption_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let header_area = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    f.render_widget(caption, caption_area);
    f.render_widget(header, header_area);

    let list_area = Rect::new(
        inner.x,
        inner.y + header_rows,
        inner.width,
        inner.height - header_rows,
    );

    // Slice the sorted row set by the scroll offset so arrow keys
    // walk the view without losing the stable A→Z order.
    let total_rows = rows.len();
    let budget = (list_area.height as usize).min(total_rows);
    let max_scroll = total_rows.saturating_sub(budget);
    let scroll = app.tokens_cursor.index.min(max_scroll);
    let viewport = Viewport {
        start: scroll,
        end: scroll + budget,
        visible: budget,
        total: total_rows,
    };
    let content_area = viewport.content_area(list_area);

    // Text column reserves a fixed width; the sparkline absorbs the
    // rest (with a lower bound so it isn't degenerate on narrow
    // panes).
    let text_width: u16 = 94;
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(text_width.min(content_area.width.saturating_sub(12))),
            Constraint::Min(10),
        ])
        .split(content_area);
    let text_col = chunks[0];
    let spark_col = chunks[1];

    let now = chrono::Utc::now();
    for (idx, trend) in rows.into_iter().skip(scroll).take(budget).enumerate() {
        let y = content_area.y + idx as u16;
        let row_text = Rect::new(text_col.x, y, text_col.width, 1);
        let row_spark = Rect::new(spark_col.x, y, spark_col.width, 1);

        let total = trend.total.total() as f64;
        let input = crate::tokens::format_token_count(trend.total.input as f64);
        let output = crate::tokens::format_token_count(trend.total.output as f64);
        let cache = crate::tokens::format_token_count(
            (trend.total.cache_read + trend.total.cache_creation) as f64,
        );
        let turns = trend
            .agents
            .iter()
            .flat_map(|a| a.buckets.iter())
            .map(|b| b.turns)
            .sum::<i64>();
        let turns_display = if turns > 0 {
            turns.to_string()
        } else {
            "-".to_owned()
        };
        let model = if trend.latest_model.is_empty() {
            trend
                .agents
                .iter()
                .find(|a| !a.latest_model.is_empty())
                .map(|a| a.latest_model.clone())
                .unwrap_or_else(|| "-".to_owned())
        } else {
            trend.latest_model.clone()
        };
        let last = trend
            .last_activity
            .map(|ts| format_last_activity(now, ts))
            .unwrap_or_else(|| "-".to_owned());
        let style = if max_total > 0.0 && total >= max_total * 0.8 {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };

        let row = format!(
            " {:<24} {:<14} {:<9} {:<9} {:<9} {:<7} {:<12}",
            crate::tasks::truncate(&trend.workspace, 24),
            crate::tasks::truncate(&short_model(&model), 14),
            input,
            output,
            cache,
            turns_display,
            last,
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                crate::tasks::truncate(&row, text_col.width as usize),
                style,
            )),
            row_text,
        );

        // Bucket series → per-row sparkline. `UsageBucket.turns` would
        // spike during an active session so the sparkline is driven by
        // `totals.total()` instead (token volume per bucket).
        let series: Vec<u64> = trend
            .buckets
            .iter()
            .map(|b| b.totals.total().max(0) as u64)
            .collect();
        if series.iter().any(|v| *v > 0) {
            let spark = Sparkline::default()
                .data(&series)
                .style(Style::default().fg(Color::Cyan));
            f.render_widget(spark, row_spark);
        } else {
            f.render_widget(
                Paragraph::new(" (flat)").style(Style::default().add_modifier(Modifier::DIM)),
                row_spark,
            );
        }
    }

    render_scrollbar(f, list_area, viewport);
}

/// Compact model label — strips vendor prefixes so names like
/// `claude-sonnet-4-6` and `gpt-5.1-codex` fit into a narrow column.
fn short_model(model: &str) -> String {
    model
        .trim_start_matches("anthropic/")
        .trim_start_matches("openai/")
        .to_owned()
}

/// Human-friendly "last seen" label relative to `now`. Falls back to
/// a YYYY-MM-DD stamp for anything older than a day so the column
/// stays parseable at a glance.
fn format_last_activity(now: chrono::DateTime<chrono::Utc>, ts: chrono::DateTime<chrono::Utc>) -> String {
    let delta = now.signed_duration_since(ts);
    if delta.num_seconds() < 0 {
        return "just now".to_owned();
    }
    let secs = delta.num_seconds();
    if secs < 60 {
        return format!("{secs}s ago");
    }
    if secs < 60 * 60 {
        return format!("{}m ago", secs / 60);
    }
    if secs < 24 * 60 * 60 {
        return format!("{}h ago", secs / 3600);
    }
    ts.format("%Y-%m-%d").to_string()
}

/// Tasks list half of the body. The detail pane is rendered
/// separately under `draw_detail_pane` so both halves benefit from
/// the uniform list/detail layout used by every other tab.
fn draw_tasks_list_only(f: &mut Frame, area: Rect, app: &App) {
    let filtered = app.filtered_tasks();
    if app.tasks.is_empty() {
        let para = Paragraph::new("  (no tasks yet)")
            .style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, area);
        return;
    }
    draw_tasks_list(f, area, app, &filtered);
}

/// Tasks detail half. Slots into the body's detail pane so long
/// logs + activity history get the full width instead of the
/// cramped column they had under the horizontal split.
fn draw_task_detail_only(f: &mut Frame, area: Rect, app: &App) {
    let filtered = app.filtered_tasks();
    draw_task_detail(f, area, app, &filtered);
}

fn draw_tasks_list(f: &mut Frame, area: Rect, app: &App, filtered: &[ax_proto::types::Task]) {
    // No outer block — the body frame already draws the border + tab
    // strip. A 3-line in-pane header carries the filter label, count,
    // and column names so operators still have context while the
    // cursor walks the rows.
    if area.width == 0 || area.height == 0 {
        return;
    }
    let header_height = area.height.min(3);
    let header_area = Rect::new(area.x, area.y, area.width, header_height);
    let body_area = Rect::new(
        area.x,
        area.y + header_height,
        area.width,
        area.height.saturating_sub(header_height),
    );
    let inner_width = area.width as usize;

    let summary = crate::tasks::summarize_tasks(&app.tasks);
    let title_line = format!(
        "tasks · {} · {}/{}",
        app.task_filter.label(),
        filtered.len().min(app.task_cursor.index.saturating_add(1)),
        filtered.len(),
    );
    let header_lines = vec![
        Line::from(Span::styled(
            crate::tasks::truncate(&title_line, inner_width.max(1)),
            Style::default()
                .fg(if app.focus == Focus::List {
                    Color::Cyan
                } else {
                    Color::White
                })
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            crate::tasks::truncate(&format_task_summary_compact(&summary), inner_width.max(1)),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            crate::tasks::truncate(
                "ID       STATE         OWNER        TITLE",
                inner_width.max(1),
            ),
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];
    let visible_headers: Vec<Line> = header_lines
        .into_iter()
        .take(header_height as usize)
        .collect();
    f.render_widget(Paragraph::new(visible_headers), header_area);

    if filtered.is_empty() {
        if body_area.height > 0 {
            let para = Paragraph::new("  (no tasks match current filter — press f to cycle)")
                .style(Style::default().add_modifier(Modifier::DIM));
            f.render_widget(para, body_area);
        }
        return;
    }

    let viewport = compute_viewport(
        filtered.len(),
        app.task_cursor.index,
        body_area.height as usize,
    );
    let rows_area = viewport.content_area(body_area);
    let rows_width = rows_area.width as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(viewport.visible);
    for (idx, task) in filtered[viewport.start..viewport.end].iter().enumerate() {
        let absolute = viewport.start + idx;
        let style = if absolute == app.task_cursor.index {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format_task_row(task, rows_width),
            style,
        )));
    }
    let para = Paragraph::new(lines);
    f.render_widget(para, rows_area);
    render_scrollbar(f, body_area, viewport);
}

fn draw_task_detail(f: &mut Frame, area: Rect, app: &App, filtered: &[ax_proto::types::Task]) {
    // Body detail pane already provides the surrounding block —
    // render plain content here.
    if area.width == 0 || area.height == 0 {
        return;
    }
    let inner_width = area.width as usize;
    let inner_height = area.height as usize;

    let Some(task) = filtered.get(app.task_cursor.index) else {
        let para = Paragraph::new("  (no task selected)")
            .style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, area);
        return;
    };

    // `build_detail_lines` over-produces; map the shared `detail_scroll`
    // onto the line window so ↑/↓ walk long logs without needing a
    // dedicated cursor.
    let all_lines = build_detail_lines(task, &app.messages, inner_width, usize::MAX);
    let total = all_lines.len();
    let visible = inner_height.min(total);
    let max_scroll = total.saturating_sub(visible);
    let scroll = app.detail_scroll.index.min(max_scroll);
    let end = (scroll + visible).min(total);
    let window: Vec<Line> = all_lines.into_iter().skip(scroll).take(end - scroll).collect();
    f.render_widget(Paragraph::new(window), area);
}

/// Agent detail pane. Reads off `selected_workspace` and assembles a
/// compact dashboard: name/status, reconcile notes, live token
/// readings, and the tail of the tmux capture so operators don't
/// have to flip tabs to peek at what the agent is doing.
fn draw_agents_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(workspace) = app.selected_workspace().map(ToOwned::to_owned) else {
        let para = Paragraph::new("  (select an agent with ↑/↓ to see its detail)")
            .style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, area);
        return;
    };
    let info = app.workspace_infos.get(&workspace);
    let trend = app.usage_trends.get(&workspace);
    let capture = app
        .captures
        .entries
        .get(&workspace)
        .map_or("", |e| e.content.as_str());
    let session = app
        .sessions
        .iter()
        .find(|s| s.workspace == workspace);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!("workspace  "),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::styled(
            workspace.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    let status_label = info
        .map(|w| agent_status_str(&w.status).to_owned())
        .unwrap_or_else(|| "offline".to_owned());
    lines.push(Line::from(vec![
        Span::styled(
            "status     ".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(status_label),
    ]));
    if let Some(info) = info {
        if !info.status_text.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    "note       ".to_owned(),
                    Style::default().add_modifier(Modifier::DIM),
                ),
                Span::raw(info.status_text.clone()),
            ]));
        }
    }
    if let Some(s) = session {
        lines.push(Line::from(vec![
            Span::styled(
                "tmux       ".to_owned(),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::raw(format!(
                "{} · {} window{}",
                s.name,
                s.windows,
                if s.windows == 1 { "" } else { "s" }
            )),
        ]));
    }
    if let Some(t) = trend.filter(|t| t.available) {
        let total_all = (t.total.cache_read + t.total.cache_creation + t.total.input) as f64;
        lines.push(Line::from(vec![
            Span::styled(
                "tokens     ".to_owned(),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::raw(format!(
                "↑{} · ↓{} · Σ{}",
                crate::tokens::format_token_count(t.total.input as f64),
                crate::tokens::format_token_count(t.total.output as f64),
                crate::tokens::format_token_count(total_all),
            )),
        ]));
    }
    if !capture.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "recent tmux:".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        )));
        let rows_budget = area.height.saturating_sub(lines.len() as u16).saturating_sub(1) as usize;
        if rows_budget > 0 {
            for line in
                crate::captures::recent_wrapped_lines(capture, rows_budget, area.width as usize)
            {
                lines.push(Line::from(Span::raw(line)));
            }
        }
    }

    apply_detail_scroll(f, area, app, lines);
}

/// Messages detail. Shows the full content of the message at the
/// current scroll tail so operators can read wrapped long messages
/// without leaving the tab. When no messages exist, renders a dim
/// placeholder so the pane isn't suspiciously empty.
fn draw_messages_detail(f: &mut Frame, area: Rect, app: &App) {
    if app.messages.is_empty() {
        let para = Paragraph::new("  (no messages yet)")
            .style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, area);
        return;
    }
    // `messages_cursor.index` is "entries from tail"; resolve to an
    // absolute index so the detail tracks the list cursor as it walks
    // back into history.
    let scroll = app.messages_cursor.index.min(app.messages.len().saturating_sub(1));
    let idx = app.messages.len() - 1 - scroll;
    let entry = &app.messages[idx];

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "time   ".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(entry.timestamp.format("%Y-%m-%d %H:%M:%S").to_string()),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            "from   ".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(entry.from.clone()),
        Span::raw("  →  "),
        Span::raw(entry.to.clone()),
    ]));
    if !entry.task_id.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "task   ".to_owned(),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::raw(entry.task_id.clone()),
        ]));
    }
    lines.push(Line::from(""));
    for raw in entry.content.lines() {
        lines.push(Line::from(Span::raw(raw.to_owned())));
    }

    apply_detail_scroll(f, area, app, lines);
}

/// Tokens detail. Resolves the workspace at the current scroll
/// position and shows the token aggregate plus bucket range. Simple
/// by design — richer per-agent/per-model breakdowns can land once
/// the underlying trend payload surfaces them.
fn draw_tokens_detail(f: &mut Frame, area: Rect, app: &App) {
    let mut rows: Vec<&ax_proto::usage::WorkspaceTrend> = app
        .usage_trends
        .values()
        .filter(|t| t.available && t.total.total() > 0)
        .collect();
    if rows.is_empty() {
        let para = Paragraph::new("  (no token activity yet)")
            .style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, area);
        return;
    }
    rows.sort_by(|a, b| a.workspace.cmp(&b.workspace));
    let idx = app.tokens_cursor.index.min(rows.len().saturating_sub(1));
    let t = rows[idx];

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "workspace  ".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::styled(
            t.workspace.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    if !t.latest_model.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "model      ".to_owned(),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::raw(short_model(&t.latest_model)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled(
            "input      ".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(crate::tokens::format_token_count(t.total.input as f64)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            "output     ".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(crate::tokens::format_token_count(t.total.output as f64)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            "cache read ".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(crate::tokens::format_token_count(t.total.cache_read as f64)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            "cache creat".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(crate::tokens::format_token_count(t.total.cache_creation as f64)),
    ]));
    if let Some(last) = t.last_activity {
        lines.push(Line::from(vec![
            Span::styled(
                "last active".to_owned(),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::raw(format_last_activity(chrono::Utc::now(), last)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled(
            "buckets    ".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(format!(
            "{} × {}m window",
            t.buckets.len(),
            t.bucket_minutes
        )),
    ]));

    apply_detail_scroll(f, area, app, lines);
}

/// Stream detail. The list half already shows the live capture, so
/// this pane focuses on metadata: workspace name, session, status.
fn draw_stream_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(workspace) = app.streamed_workspace.clone() else {
        let para = Paragraph::new(
            "  (no workspace streaming yet — open the agents tab, press Enter → Stream tmux)",
        )
        .style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, area);
        return;
    };
    let info = app.workspace_infos.get(&workspace);
    let session = app
        .sessions
        .iter()
        .find(|s| s.workspace == workspace);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "mirroring  ".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::styled(
            workspace.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    if let Some(info) = info {
        lines.push(Line::from(vec![
            Span::styled(
                "status     ".to_owned(),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::raw(agent_status_str(&info.status).to_owned()),
        ]));
        if !info.status_text.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    "note       ".to_owned(),
                    Style::default().add_modifier(Modifier::DIM),
                ),
                Span::raw(info.status_text.clone()),
            ]));
        }
    }
    if let Some(s) = session {
        lines.push(Line::from(vec![
            Span::styled(
                "session    ".to_owned(),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::raw(format!(
                "{} · {} window{}",
                s.name,
                s.windows,
                if s.windows == 1 { "" } else { "s" }
            )),
        ]));
    }

    apply_detail_scroll(f, area, app, lines);
}

/// Windowed render for detail panes that over-produce lines. Maps
/// `app.detail_scroll.index` onto the visible slice and clamps the
/// ceiling so arrow keys can over-shoot without scrolling past the
/// last row.
fn apply_detail_scroll(f: &mut Frame, area: Rect, app: &App, lines: Vec<Line<'static>>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let total = lines.len();
    let visible = (area.height as usize).min(total);
    let max_scroll = total.saturating_sub(visible);
    let scroll = app.detail_scroll.index.min(max_scroll);
    let end = (scroll + visible).min(total);
    let window: Vec<Line<'static>> = lines.into_iter().skip(scroll).take(end - scroll).collect();
    f.render_widget(Paragraph::new(window), area);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Viewport {
    start: usize,
    end: usize,
    visible: usize,
    total: usize,
}

impl Viewport {
    fn is_scrollable(self) -> bool {
        self.total > self.visible && self.visible > 0
    }

    fn shows_scrollbar(self, area: Rect) -> bool {
        self.is_scrollable() && area.height > 0 && area.width > 2
    }

    fn content_area(self, area: Rect) -> Rect {
        if self.shows_scrollbar(area) {
            Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height)
        } else {
            area
        }
    }

    fn scroll_state(self) -> ScrollbarState {
        ScrollbarState::new(self.total)
            .position(self.start)
            .viewport_content_length(self.visible)
    }
}

/// Walk `tasks[..]` around `selected` so the cursor stays visible
/// once the list outgrows the pane (one row per task).
fn compute_viewport(total: usize, selected: usize, budget: usize) -> Viewport {
    if total == 0 || budget == 0 {
        return Viewport {
            start: 0,
            end: 0,
            visible: 0,
            total,
        };
    }
    let visible = budget.min(total);
    let start = if selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let max_start = total - visible;
    let start = start.min(max_start);
    Viewport {
        start,
        end: start + visible,
        visible,
        total,
    }
}

#[cfg(test)]
fn viewport_range(total: usize, selected: usize, budget: usize) -> (usize, usize) {
    let viewport = compute_viewport(total, selected, budget);
    (viewport.start, viewport.end)
}

fn render_scrollbar(f: &mut Frame, area: Rect, viewport: Viewport) {
    if !viewport.shows_scrollbar(area) {
        return;
    }
    let mut state = viewport.scroll_state();
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None);
    f.render_stateful_widget(scrollbar, area, &mut state);
}

fn format_task_row(task: &ax_proto::types::Task, width: usize) -> String {
    let id = crate::tasks::short_task_id(&task.id);
    let state = format_task_state(task);
    let row = format!(
        "{id:<8} {:<13} {:<12} {}",
        crate::tasks::truncate(&state, 13),
        crate::tasks::truncate(&task.assignee, 12),
        task.title,
    );
    crate::tasks::truncate(&row, width.max(1))
}

fn format_task_summary_compact(summary: &crate::tasks::TaskSummary) -> String {
    let mut parts = vec![
        format!("tot {}", summary.total),
        format!("run {}", summary.in_progress),
        format!("pend {}", summary.pending),
        format!("stale {}", summary.stale),
    ];
    if summary.failed > 0 {
        parts.push(format!("fail {}", summary.failed));
    }
    if summary.completed > 0 {
        parts.push(format!("done {}", summary.completed));
    }
    if summary.queued_messages > 0 {
        parts.push(format!("msg {}", summary.queued_messages));
    }
    if summary.diverged > 0 {
        parts.push(format!("div {}", summary.diverged));
    }
    if summary.urgent_or_high > 0 {
        parts.push(format!("hi {}", summary.urgent_or_high));
    }
    if summary.cancelled > 0 {
        parts.push(format!("cancel {}", summary.cancelled));
    }
    parts.join(" · ")
}

fn format_task_state(task: &ax_proto::types::Task) -> String {
    let base = match task.status {
        ax_proto::types::TaskStatus::Pending => "pending",
        ax_proto::types::TaskStatus::InProgress => "running",
        ax_proto::types::TaskStatus::Blocked => "blocked",
        ax_proto::types::TaskStatus::Completed => "done",
        ax_proto::types::TaskStatus::Failed => "failed",
        ax_proto::types::TaskStatus::Cancelled => "cancelled",
    };
    if crate::tasks::task_is_stale(task)
        && matches!(
            task.status,
            ax_proto::types::TaskStatus::Pending | ax_proto::types::TaskStatus::InProgress
        )
    {
        format!("{base} stale")
    } else {
        base.to_owned()
    }
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
        (focus_footer_hint(app), Style::default().add_modifier(Modifier::DIM))
    };
    let footer = Paragraph::new(text).style(style);
    f.render_widget(footer, area);
}

/// Pick a context-aware hint line based on the focused pane so the
/// footer shows the keys that actually do something right now. The
/// list half gets view-specific hints; the detail half gets a
/// uniform scroll/esc line since every detail uses the shared
/// `detail_scroll` cursor.
fn focus_footer_hint(app: &App) -> String {
    let base = "[/] pane · Tab/1-5 view · f filter · ? help · q quit";
    let scoped = match app.focus {
        Focus::List => match app.stream {
            StreamView::Agents => "↑↓/jk agent · enter actions",
            StreamView::Tasks => "↑↓/jk task",
            StreamView::Messages | StreamView::Tokens => {
                "↑↓/jk scroll · g/G head/tail"
            }
            StreamView::Stream => "live tmux mirror",
        },
        Focus::Detail => "↑↓/jk scroll · g reset · esc list",
    };
    format!("[{focus}] {scoped} · {base}", focus = app.focus.label())
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
    use ax_proto::types::{Task, TaskStartMode, TaskStatus};

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

    #[test]
    fn compact_task_summary_prioritizes_short_labels() {
        let summary = crate::tasks::TaskSummary {
            total: 12,
            pending: 3,
            in_progress: 2,
            completed: 4,
            failed: 1,
            stale: 2,
            diverged: 1,
            queued_messages: 9,
            urgent_or_high: 2,
            ..crate::tasks::TaskSummary::default()
        };
        assert_eq!(
            format_task_summary_compact(&summary),
            "tot 12 · run 2 · pend 3 · stale 2 · fail 1 · done 4 · msg 9 · div 1 · hi 2"
        );
    }

    #[test]
    fn compact_task_state_marks_active_stale_tasks() {
        let mut task = mock_task();
        task.status = TaskStatus::InProgress;
        task.stale_after_seconds = 1;
        task.updated_at = chrono::Utc::now() - chrono::Duration::seconds(5);
        assert_eq!(format_task_state(&task), "running stale");
    }

    fn mock_task() -> Task {
        let now = chrono::Utc::now();
        Task {
            id: "abc".into(),
            title: "task title".into(),
            description: String::new(),
            assignee: "alpha".into(),
            created_by: "orch".into(),
            parent_task_id: String::new(),
            child_task_ids: Vec::new(),
            version: 1,
            status: TaskStatus::Pending,
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
}
