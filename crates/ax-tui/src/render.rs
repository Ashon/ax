//! ratatui draw routine. Splits the screen into a top `agents` panel
//! (project tree + live sessions) and a body pane underneath for
//! messages / tasks / tokens or a full-screen tmux capture mirror.

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
            Constraint::Length(1), // header
            Constraint::Min(1),    // agents + tabs + content
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(f, chunks[0], app);

    let streaming = app.streamed_workspace.is_some();
    let agents_h = compute_agents_height(app, chunks[1].height, streaming);
    // No standalone tab row — the body block embeds the tab strip
    // inside its top border (see `tabs_title`), so the middle region
    // splits cleanly into agents + body.
    let middle = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(agents_h), Constraint::Min(1)])
        .split(chunks[1]);
    let agents_area = middle[0];
    let content_area = middle[1];
    draw_agents(f, agents_area, app);
    draw_body(f, content_area, app);

    if app.quick_actions.open {
        draw_quick_actions(f, area, agents_area, app);
    }

    draw_footer(f, chunks[2], app);
}

/// Clamp the agents pane so it shows every row when possible but
/// never starves the content pane below it. Overflow rows scroll
/// within the pane; the reserved budget accounts for the body
/// block's border (2) so it never collapses to a single row.
fn compute_agents_height(app: &App, middle_h: u16, _streaming: bool) -> u16 {
    let reserved = 3;
    let desired = (app.agent_entries.len() as u16).saturating_add(3).max(5);
    let cap = middle_h.saturating_sub(reserved).max(3);
    desired.min(cap)
}

/// Context-menu style overlay: drops down from the selected agent
/// row with a small indent so it reads as a popup on that row.
/// Clamps into the frame so it never runs off the edge.
fn draw_quick_actions(f: &mut Frame, frame: Rect, agents_area: Rect, app: &App) {
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
    // the top). The panel block contributes a 1-row top border plus a
    // 1-row column header before the list starts.
    let list_area_height = agents_area.height.saturating_sub(3);
    let viewport = compute_viewport(
        app.agent_entries.len(),
        app.selected_entry,
        list_area_height as usize,
    );
    let list_y = agents_area.y.saturating_add(2);
    let rel = (app.selected_entry.saturating_sub(viewport.start)) as u16;
    let selected_row = list_y.saturating_add(rel);

    let anchor_x = agents_area.x + 2;
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
    let text = format!(
        "ax watch — daemon: {daemon} · agents: {} · sessions: {}",
        app.workspace_infos.len(),
        app.sessions.len(),
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

fn draw_agents(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border_style(app, Focus::Agents))
        .title(" agents ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if app.agent_entries.is_empty() {
        let empty = Paragraph::new(
            "No active agents. Run `ax up` in a project directory with .ax/config.yaml.",
        )
        .wrap(Wrap { trim: true });
        f.render_widget(empty, inner);
        return;
    }

    let cols = AgentColumns::fit(inner.width);
    let header_area = Rect::new(inner.x, inner.y, inner.width, 1);
    draw_agents_header(f, header_area, &cols);

    let list_area = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    if list_area.height == 0 {
        return;
    }
    // Slice the entry list so the selection stays inside the visible
    // rows. A scrollbar makes off-screen overflow visible instead of
    // relying on inline hint markers alone.
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
    if let Some(workspace) = app.streamed_workspace.clone() {
        draw_stream_single(f, area, app, &workspace);
        return;
    }
    // Outer body block. Tab strip sits on the top border so we don't
    // burn a row on a standalone tab row. Sub-views render into the
    // inner area without drawing their own outer border.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border_style(app, Focus::Body))
        .title(tabs_title(app));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    match app.stream {
        StreamView::Messages => draw_messages(f, inner, app),
        StreamView::Tasks => draw_tasks(f, inner, app),
        StreamView::Tokens => draw_tokens(f, inner, app),
    }
}

/// Build the body block's title as a tab strip. The line sits on the
/// top border of whichever body sub-view is active, so we don't burn
/// an extra row on a standalone tab strip above the pane. Styles
/// follow the same focus/selection matrix as a dedicated Tabs
/// widget: when Tabs focus is active, the highlighted tab turns
/// cyan bold; otherwise it falls back to white bold with dim
/// siblings. Dots between tabs imitate `symbols::DOT`.
fn tabs_title(app: &App) -> Line<'static> {
    let focused = app.focus == Focus::Tabs;
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

fn draw_stream_single(f: &mut Frame, area: Rect, app: &mut App, workspace: &str) {
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
        let loading_area = centered_loading_area(inner);
        let throbber = status_throbber(
            "waiting for tmux capture…".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        );
        f.render_stateful_widget(throbber, loading_area, &mut app.throbber_state);
        return;
    }

    let rows = inner.height as usize;
    let width = inner.width as usize;
    let lines: Vec<Line> = crate::captures::recent_wrapped_lines(capture, rows, width)
        .into_iter()
        .map(|line| Line::from(Span::raw(line)))
        .collect();
    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
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
    let inner_width = area.width as usize;
    let inner_height = area.height as usize;

    if app.messages.is_empty() {
        let para =
            Paragraph::new("  (no messages yet)").style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(para, area);
        return;
    }

    let start = app.messages.len().saturating_sub(inner_height.max(1));
    let lines: Vec<Line> = app.messages[start..]
        .iter()
        .map(|entry| Line::from(Span::raw(format_message_line(entry, inner_width.max(1)))))
        .collect();
    f.render_widget(Paragraph::new(lines), area);
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

    // Text column reserves a fixed width; the sparkline absorbs the
    // rest (with a lower bound so it isn't degenerate on narrow
    // panes).
    let text_width: u16 = 94;
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(text_width.min(list_area.width.saturating_sub(12))),
            Constraint::Min(10),
        ])
        .split(list_area);
    let text_col = chunks[0];
    let spark_col = chunks[1];

    let now = chrono::Utc::now();
    let budget = (list_area.height as usize).min(rows.len());
    for (idx, trend) in rows.into_iter().take(budget).enumerate() {
        let y = list_area.y + idx as u16;
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

fn draw_tasks(f: &mut Frame, area: Rect, app: &App) {
    // `area` is the body block's inner rect; `draw_body` already
    // painted the outer border + tab-strip title. The list/detail
    // split lives inside.
    let filtered = app.filtered_tasks();
    if app.tasks.is_empty() {
        let para = Paragraph::new("  (no tasks yet)")
            .style(Style::default().add_modifier(Modifier::DIM));
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border_style(app, Focus::Body))
        .title(format!(
            " tasks {} {}/{} ",
            app.task_filter.label(),
            filtered.len().min(app.task_selected.saturating_add(1)),
            filtered.len(),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let summary = crate::tasks::summarize_tasks(&app.tasks);
    let header_height = inner.height.min(2);
    let header_area = Rect::new(inner.x, inner.y, inner.width, header_height);
    let body_area = Rect::new(
        inner.x,
        inner.y + header_height,
        inner.width,
        inner.height.saturating_sub(header_height),
    );
    let inner_width = inner.width as usize;

    let header_lines = vec![
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

    let viewport = compute_viewport(filtered.len(), app.task_selected, body_area.height as usize);
    let rows_area = viewport.content_area(body_area);
    let rows_width = rows_area.width as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(viewport.visible);
    for (idx, task) in filtered[viewport.start..viewport.end].iter().enumerate() {
        let absolute = viewport.start + idx;
        let style = if absolute == app.task_selected {
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
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border_style(app, Focus::Body))
        .title(" detail ");

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
    } else if app.streamed_workspace.is_some() {
        (
            "esc exit stream · q quit".to_owned(),
            Style::default().add_modifier(Modifier::DIM),
        )
    } else {
        (focus_footer_hint(app), Style::default().add_modifier(Modifier::DIM))
    };
    let footer = Paragraph::new(text).style(style);
    f.render_widget(footer, area);
}

/// Pick a context-aware hint line based on the focused panel so the
/// footer shows the keys that actually do something right now.
fn focus_footer_hint(app: &App) -> String {
    let base = "[/] panel · 1-3 tab · f filter · q quit";
    let scoped = match app.focus {
        Focus::Agents => "↑↓/jk agent · enter actions",
        Focus::Tabs => "Tab/←→ tab · ↓ body · ↑ agents",
        Focus::Body => match app.stream {
            StreamView::Tasks => "↑↓/jk task · ←→ tab · esc tabs",
            _ => "←→ tab · esc tabs",
        },
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
