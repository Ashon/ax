//! ratatui draw routine. Body is a tab strip over a vertical
//! list/detail split: every tab — agents, messages, tasks, tokens,
//! stream — owns both a list renderer (top) and a detail renderer
//! (bottom). No standalone agents pane anymore; fleet visibility
//! moves to the header status bar when the active tab isn't agents.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Sparkline, Wrap,
};
use ratatui::Frame;
use throbber_widgets_tui::{Throbber, WhichUse, BRAILLE_SIX};

use ax_config::ProjectNode;
use ax_proto::types::{AgentStatus, WorkspaceGitStatus};

use crate::agents::AgentEntry;
use crate::state::{AgentDetailTab, App, Focus};
use crate::stream::StreamView;
use crate::theme::{self, Severity};

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
        draw_help(f, area, app);
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
    let list_h = (inner.height * 45 / 100)
        .max(3)
        .min(inner.height.saturating_sub(3));
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
fn draw_help(f: &mut Frame, frame: Rect, app: &App) {
    let stream_pinned = app.streamed_workspace.is_some();
    let tab_keys = if stream_pinned { "1-5" } else { "1-4" };
    let tab_desc = if stream_pinned {
        "agents · messages · tasks · tokens · stream"
    } else {
        "agents · messages · tasks · tokens"
    };
    let mut sections: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (
            "global",
            vec![
                ("?", "toggle this help"),
                ("q / ctrl-c", "quit"),
                ("[ / ]", "switch pane (list ↔ detail)"),
                ("Tab / Shift-Tab", "cycle visible tab"),
                (tab_keys, tab_desc),
                ("f", "cycle task filter"),
            ],
        ),
        (
            "list · agents",
            vec![
                ("↑ ↓ / j k", "move agent cursor"),
                ("Enter", "open action menu"),
                ("wheel", "scroll list"),
            ],
        ),
        (
            "list · tasks",
            vec![
                ("↑ ↓ / j k", "move selected task"),
                ("wheel", "scroll list"),
            ],
        ),
        (
            "list · messages / tokens",
            vec![
                ("↑ ↓ / j k", "scroll"),
                ("PgUp / PgDn", "scroll by page"),
                ("g / G", "head / tail"),
                ("wheel", "scroll"),
            ],
        ),
        (
            "detail",
            vec![
                ("↑ ↓ / j k", "scroll detail"),
                ("h / l", "cycle agent detail tab (agents only)"),
                ("PgUp / PgDn", "scroll by page"),
                ("g", "top"),
                ("Esc", "back to list"),
            ],
        ),
        (
            "action menu",
            vec![
                ("↑ ↓", "select action"),
                ("Enter", "run (re-press to confirm destructive ops)"),
                ("Esc", "close"),
            ],
        ),
    ];
    if stream_pinned {
        sections.insert(
            4,
            (
                "list · stream",
                vec![
                    ("↑ ↓ / j k", "scroll / freeze"),
                    ("PgUp / PgDn", "scroll by page"),
                    ("g / G", "top / follow tail"),
                ],
            ),
        );
    }

    let key_col = 18usize;
    let mut lines: Vec<Line> = Vec::new();
    for (idx, (section, rows)) in sections.iter().enumerate() {
        if idx > 0 {
            // Blank separator between sections so the cheatsheet reads
            // as a vertically-stacked set of groups rather than one
            // long table.
            lines.push(Line::from(Span::raw("")));
        }
        lines.push(Line::from(Span::styled(
            format!(" {section}"),
            theme::accent_bold(),
        )));
        for (key, desc) in rows {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<width$}", width = key_col), theme::strong()),
                Span::raw(*desc),
            ]));
        }
    }

    let total_rows = lines.len() as u16;
    let height = (total_rows + 2).min(frame.height.saturating_sub(2)).max(6);
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

    let mut visible: Vec<Line> = lines.into_iter().take(inner.height as usize).collect();
    if total_rows as usize > inner.height as usize && !visible.is_empty() {
        *visible.last_mut().expect("visible is non-empty") = Line::from(Span::styled(
            "  ... resize terminal for more help",
            theme::muted(),
        ));
    }
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
    let target = if app.quick_actions.target_task_id.is_empty() {
        if app.quick_actions.target_workspace.is_empty() {
            app.selected_workspace().unwrap_or("").to_owned()
        } else {
            app.quick_actions.target_workspace.clone()
        }
    } else {
        crate::tasks::short_task_id(&app.quick_actions.target_task_id)
    };
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

    let selected_row = if app.quick_actions.target_task_id.is_empty() {
        // Reproduce the agents-panel layout so the popup appears
        // below the row under the cursor. The list pane reserves 1
        // row for its column header before agent rows start.
        let rows_height = list_area.height.saturating_sub(1);
        let viewport = compute_viewport(
            app.agent_entries.len(),
            app.selected_entry,
            rows_height as usize,
        );
        let rows_y = list_area.y.saturating_add(1);
        let rel = (app.selected_entry.saturating_sub(viewport.start)) as u16;
        rows_y.saturating_add(rel)
    } else {
        // Task action overlay anchors to the selected task row. The
        // tasks list reserves up to 3 rows for filter/summary/header.
        let header_height = list_area.height.min(3);
        let body_height = list_area.height.saturating_sub(header_height);
        let filtered_len = app.filtered_tasks().len();
        let selected = app.task_cursor.index.min(filtered_len.saturating_sub(1));
        let viewport = compute_viewport(filtered_len, selected, body_height as usize);
        let rows_y = list_area.y.saturating_add(header_height);
        let rel = selected.saturating_sub(viewport.start) as u16;
        rows_y.saturating_add(rel)
    };

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

    let title = format!(" {target} actions ");
    let block = Block::default().borders(Borders::ALL).title(title);

    let mut lines: Vec<Line> = Vec::with_capacity(app.quick_actions.actions.len() + 2);
    if app.quick_actions.confirm {
        let action = app.quick_actions.current().map(|a| a.id);
        let prompt = action
            .map(|id| id.confirm_prompt(&target))
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            prompt,
            theme::severity_bold(Severity::Warning),
        )));
        lines.push(Line::from(Span::styled(
            "enter to confirm · esc to cancel",
            theme::muted(),
        )));
    } else {
        for (idx, action) in app.quick_actions.actions.iter().enumerate() {
            let cursor = if idx == app.quick_actions.selected {
                "▸ "
            } else {
                "  "
            };
            let style = if idx == app.quick_actions.selected {
                theme::selection(true)
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
            theme::muted(),
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
                ax_proto::types::TaskStatus::Pending
                    | ax_proto::types::TaskStatus::InProgress
                    | ax_proto::types::TaskStatus::Blocked
            )
        })
        .count();
    let blocked_tasks = app
        .tasks
        .iter()
        .filter(|t| matches!(t.status, ax_proto::types::TaskStatus::Blocked))
        .count();
    let blocked_segment = if blocked_tasks > 0 {
        format!(" · blocked: {blocked_tasks}")
    } else {
        String::new()
    };
    // Collapse segments so the line stays under a single terminal row
    // even on narrow windows. `agents: 0/0` and an empty task set are
    // common on a freshly-booted repo, so keep them visible as a cue
    // that the surface is wired up.
    let text = format!(
        "ax · daemon: {daemon} · agents: {online}/{total_agents} · tasks: {active_tasks} active / {total_tasks}{blocked_segment} · sessions: {sessions} · filter: {filter}",
        total_tasks = app.tasks.len(),
        sessions = app.sessions.len(),
        filter = app.task_filter.label(),
    );
    if app.daemon_running {
        let throbber = status_throbber(text, theme::strong());
        let mut state = app.throbber_state.clone();
        f.render_stateful_widget(throbber, area, &mut state);
    } else {
        let header = Paragraph::new(text).style(theme::strong());
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

/// Column widths for the agents table. INFO flexes to fill the
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
        // INFO absorbs status/reconcile notes while NAME carries
        // group-level git summaries next to the group label.
        let compact = width < 104;
        let name = if compact { 34 } else { 44 };
        let state = if compact { 8 } else { 11 };
        let up = if compact { 5 } else { 7 };
        let down = if compact { 5 } else { 7 };
        let cost = if compact { 6 } else { 8 };
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
    let mut spans = Vec::new();
    push_padded_span(&mut spans, "NAME", cols.name, theme::column_header());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "STATE", cols.state, theme::column_header());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "UP", cols.up, theme::traffic_up());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "DOWN", cols.down, theme::traffic_down());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "COST", cols.cost, theme::cost());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "INFO", cols.info, theme::column_header());
    let para = Paragraph::new(Line::from(spans));
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
        return agent_group_row(idx, entry, app, cols, &indent);
    }

    let live = entry.session_index.is_some();
    let is_selected = idx == app.selected_entry;
    let cursor = if is_selected { "▸" } else { " " };
    let marker = if live { "●" } else { "○" };

    let info_opt = app.workspace_infos.get(&entry.workspace);
    let name_raw = format!("{cursor} {indent}{marker} {}", entry.label);

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
            format!(
                "↑{}",
                crate::tokens::format_token_count(t.total.input as f64)
            ),
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

    let selected_style = is_selected.then(|| theme::selection(app.focus == Focus::List));
    let name_style = selected_style.unwrap_or_else(|| {
        if live {
            theme::workspace(entry.level)
        } else {
            theme::disabled()
        }
    });
    let state_style = selected_style.unwrap_or_else(|| {
        if live {
            if state_raw.contains("running") {
                theme::running()
            } else {
                theme::idle()
            }
        } else {
            info_opt.map_or_else(theme::disabled, |w| theme::agent_status(&w.status))
        }
    });
    let up_style = selected_style.unwrap_or_else(|| metric_style(&up_raw, theme::traffic_up()));
    let down_style =
        selected_style.unwrap_or_else(|| metric_style(&down_raw, theme::traffic_down()));
    let cost_style = selected_style.unwrap_or_else(|| metric_style(&cost_raw, theme::cost()));
    let info_style = selected_style.unwrap_or_else(|| {
        if info_raw.is_empty() {
            theme::muted()
        } else if entry.reconcile.is_empty() {
            theme::info()
        } else {
            theme::severity(Severity::Warning)
        }
    });
    let gap_style = selected_style.unwrap_or_else(Style::default);

    let mut spans = Vec::new();
    push_padded_span(&mut spans, &name_raw, cols.name, name_style);
    push_gap_with(&mut spans, gap_style);
    push_padded_span(&mut spans, &state_raw, cols.state, state_style);
    push_gap_with(&mut spans, gap_style);
    push_padded_span(&mut spans, &up_raw, cols.up, up_style);
    push_gap_with(&mut spans, gap_style);
    push_padded_span(&mut spans, &down_raw, cols.down, down_style);
    push_gap_with(&mut spans, gap_style);
    push_padded_span(&mut spans, &cost_raw, cols.cost, cost_style);
    push_gap_with(&mut spans, gap_style);
    push_padded_span(&mut spans, &info_raw, cols.info, info_style);
    ListItem::new(Line::from(spans))
}

fn agent_group_row<'a>(
    idx: usize,
    entry: &'a AgentEntry,
    app: &'a App,
    cols: &AgentColumns,
    indent: &str,
) -> ListItem<'a> {
    let label = format!("{indent}{}", entry.label);
    let git = group_git_summary(idx, app, cols.name >= 44);
    let mut spans = Vec::new();
    let label_style = theme::workspace(entry.level).add_modifier(ratatui::style::Modifier::BOLD);
    if let Some((summary, style)) = git {
        let suffix = format!(" git {summary}");
        push_padded_split_span(&mut spans, &label, &suffix, cols.name, label_style, style);
    } else {
        push_padded_span(&mut spans, label, cols.name, label_style);
    }
    push_gap(&mut spans);
    push_padded_span(&mut spans, "", cols.state, theme::muted());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "", cols.up, theme::muted());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "", cols.down, theme::muted());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "", cols.cost, theme::muted());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "", cols.info, theme::muted());
    ListItem::new(Line::from(spans))
}

fn group_git_summary(idx: usize, app: &App, wide: bool) -> Option<(String, Style)> {
    let group = app.agent_entries.get(idx)?;
    if !group.group {
        return None;
    }
    let mut first: Option<&WorkspaceGitStatus> = None;
    let mut mixed = false;
    for child in app.agent_entries.iter().skip(idx + 1) {
        if child.group && child.level <= group.level {
            break;
        }
        if child.group {
            continue;
        }
        if child.level < group.level {
            break;
        }
        if child.level > group.level + 1 {
            continue;
        }
        let Some(git) = app
            .workspace_infos
            .get(&child.workspace)
            .and_then(|info| info.git_status.as_ref())
        else {
            continue;
        };
        if let Some(existing) = first {
            if existing != git {
                mixed = true;
                break;
            }
        } else {
            first = Some(git);
        }
    }
    if mixed {
        return Some(("mixed".to_owned(), theme::severity(Severity::Warning)));
    }
    let git = first?;
    let state = normalized_git_state(git);
    Some((
        format_git_status_inline(git, wide),
        theme::git_state(&state),
    ))
}

fn format_git_status_inline(git: &WorkspaceGitStatus, wide: bool) -> String {
    let state = normalized_git_state(git);
    match state.as_str() {
        "non_git" => return "non-git".to_owned(),
        "inaccessible" => return "no access".to_owned(),
        "error" => return "git err".to_owned(),
        _ => {}
    }

    let changed = git.modified + git.added + git.deleted;
    if state == "clean" && changed == 0 && git.untracked == 0 {
        return "clean".to_owned();
    }
    if wide {
        return format!("changed:{changed} ?{}", git.untracked);
    }
    format!("~{changed} ?{}", git.untracked)
}

fn format_git_status_detail(git: &WorkspaceGitStatus) -> String {
    let state = normalized_git_state(git);
    match state.as_str() {
        "non_git" => return git_message("non-git", git),
        "inaccessible" => return git_message("inaccessible", git),
        "error" => return git_message("error", git),
        _ => {}
    }

    let mut out = format!(
        "{state} · modified {} · added {} · deleted {} · untracked {}",
        git.modified, git.added, git.deleted, git.untracked
    );
    if git.files_changed > 0 || git.insertions > 0 || git.deletions > 0 {
        out.push_str(&format!(
            " · diff {} files +{} -{}",
            git.files_changed, git.insertions, git.deletions
        ));
    }
    out
}

fn normalized_git_state(git: &WorkspaceGitStatus) -> String {
    let state = git.state.trim();
    if !state.is_empty() {
        return state.to_owned();
    }
    if git.modified + git.added + git.deleted + git.untracked > 0 {
        "dirty".to_owned()
    } else {
        "clean".to_owned()
    }
}

fn git_message(prefix: &str, git: &WorkspaceGitStatus) -> String {
    if git.message.trim().is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}: {}", git.message.trim())
    }
}

fn push_gap(spans: &mut Vec<Span<'static>>) {
    spans.push(Span::raw(" "));
}

fn push_gap_with(spans: &mut Vec<Span<'static>>, style: Style) {
    spans.push(Span::styled(" ", style));
}

fn push_padded_span(
    spans: &mut Vec<Span<'static>>,
    text: impl AsRef<str>,
    width: usize,
    style: Style,
) {
    spans.push(Span::styled(pad_or_trunc(text.as_ref(), width), style));
}

fn push_padded_split_span(
    spans: &mut Vec<Span<'static>>,
    label: &str,
    suffix: &str,
    width: usize,
    label_style: Style,
    suffix_style: Style,
) {
    if width == 0 {
        return;
    }

    let label_len = label.chars().count();
    let suffix_len = suffix.chars().count();
    if suffix_len >= width {
        push_padded_span(spans, format!("{label}{suffix}"), width, label_style);
        return;
    }

    let label_width = width - suffix_len;
    if label_len > label_width {
        spans.push(Span::styled(
            crate::tasks::truncate(label, label_width),
            label_style,
        ));
        spans.push(Span::styled(suffix.to_owned(), suffix_style));
        return;
    }

    spans.push(Span::styled(label.to_owned(), label_style));
    spans.push(Span::styled(suffix.to_owned(), suffix_style));
    spans.push(Span::styled(
        " ".repeat(width - label_len - suffix_len),
        label_style,
    ));
}

fn metric_style(raw: &str, style: Style) -> Style {
    if raw.trim() == "-" {
        theme::muted()
    } else {
        style
    }
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
    app.ensure_stream_view_visible();
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
    let title = detail_title(app, area.width);
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
fn detail_title(app: &App, width: u16) -> Line<'static> {
    let focus_detail = app.focus == Focus::Detail;
    if app.stream == StreamView::Agents {
        return agent_detail_title(app, focus_detail, width);
    }

    let label = match app.stream {
        StreamView::Agents => unreachable!("handled above"),
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
        theme::accent_bold()
    } else {
        theme::muted()
    };
    Line::from(Span::styled(label, style))
}

fn agent_detail_title(app: &App, focus_detail: bool, width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let Some(workspace) = app.selected_workspace() else {
        let style = if focus_detail {
            theme::accent_bold()
        } else {
            theme::muted()
        };
        return Line::from(Span::styled(" agent detail ".to_owned(), style));
    };
    let compact = width < 88;
    let title_style = if focus_detail {
        theme::accent_bold()
    } else {
        theme::muted()
    };
    spans.push(Span::styled(
        format!(" agent detail · {workspace} · "),
        title_style,
    ));
    for (idx, tab) in AgentDetailTab::ALL.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" ".to_owned(), theme::muted()));
        }
        let label = if compact {
            tab.short_label()
        } else {
            tab.label()
        };
        let style = if *tab == app.agent_detail_tab {
            theme::active_label(focus_detail)
        } else if focus_detail {
            theme::disabled()
        } else {
            theme::muted()
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    Line::from(spans)
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
    for (idx, view) in app.stream_tab_views().iter().enumerate() {
        if idx > 0 {
            // Use the same horizontal glyph as the surrounding block
            // border so the divider melts into the top edge instead
            // of poking up as a dot.
            spans.push(Span::styled(
                format!(" {} ", symbols::line::HORIZONTAL),
                theme::muted(),
            ));
        }
        let label = format!(" {}·{} ", idx + 1, view.tab_label());
        let is_selected = *view == app.stream;
        let style = match (is_selected, focused) {
            (true, true) => theme::active_label(true),
            (true, false) => theme::active_label(false),
            (false, true) => theme::disabled(),
            (false, false) => theme::muted(),
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
        .style(theme::muted());
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
            theme::muted(),
        );
        f.render_stateful_widget(throbber, loading_area, &mut app.throbber_state);
        return;
    }

    // Give the header a single-row caption so the streaming target
    // stays identifiable even though the tab strip only says
    // "stream". Mirrors the old hijack-mode title.
    let mode = if app.stream_follow_tail {
        "follow"
    } else {
        "frozen"
    };
    let caption =
        Paragraph::new(format!("  {workspace} · tmux mirror · {mode}")).style(theme::accent_bold());
    let caption_area = Rect::new(area.x, area.y, area.width, 1);
    f.render_widget(caption, caption_area);
    let body_area = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    if body_area.height == 0 {
        return;
    }

    let width = body_area.width as usize;
    let visual_lines = crate::captures::wrapped_lines(capture, width);
    let rows = body_area.height as usize;
    let visible = rows.min(visual_lines.len());
    let max_scroll = visual_lines.len().saturating_sub(visible);
    if app.stream_follow_tail {
        app.stream_cursor.index = max_scroll;
    } else {
        app.stream_cursor.clamp(max_scroll);
        if app.stream_cursor.index == max_scroll {
            app.stream_follow_tail = true;
        }
    }
    let start = app.stream_cursor.index.min(max_scroll);
    let end = (start + visible).min(visual_lines.len());
    let lines: Vec<Line> = visual_lines[start..end]
        .iter()
        .cloned()
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
        .throbber_style(theme::accent_bold())
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
    theme::focus_border(app.focus == panel)
}

fn draw_messages(f: &mut Frame, area: Rect, app: &App) {
    // `area` is the body list pane — outer border + tab strip are
    // already painted by `draw_body`. Render raw rows into it.
    let mut list_area = area;
    if let Some(error) = &app.messages_snapshot_error {
        let warning_area = Rect::new(area.x, area.y, area.width, area.height.min(1));
        f.render_widget(
            Paragraph::new(crate::tasks::truncate(
                &format!("  snapshot error: {error}"),
                warning_area.width as usize,
            ))
            .style(theme::severity(Severity::Danger)),
            warning_area,
        );
        list_area = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );
    }
    if app.messages.is_empty() {
        if app.messages_snapshot_error.is_some() {
            return;
        }
        let para = Paragraph::new("  (no messages yet)").style(theme::muted());
        f.render_widget(para, list_area);
        return;
    }

    // Viewport keeps the selected row visible so ↑/↓ behave like a
    // standard list: cursor moves, window follows.
    let cursor = app
        .messages_cursor
        .index
        .min(app.messages.len().saturating_sub(1));
    let viewport = compute_viewport(app.messages.len(), cursor, list_area.height as usize);
    let content_area = viewport.content_area(list_area);
    let content_width = content_area.width as usize;

    let list_focused = app.focus == Focus::List;
    let lines: Vec<Line> = app.messages[viewport.start..viewport.end]
        .iter()
        .enumerate()
        .map(|(rel, entry)| {
            let absolute = viewport.start + rel;
            message_list_line(
                entry,
                content_width.max(1),
                absolute == cursor,
                list_focused,
            )
        })
        .collect();
    f.render_widget(Paragraph::new(lines), content_area);
    render_scrollbar(f, list_area, viewport);
}

fn message_list_line(
    entry: &ax_daemon::HistoryEntry,
    width: usize,
    selected: bool,
    focused: bool,
) -> Line<'static> {
    if let Some(activity) = mcp_tool_activity(entry) {
        return mcp_tool_activity_line(entry, &activity, width, selected, focused);
    }

    let selected_style = selected.then(|| theme::selection(focused));
    let time = format!(" {}", entry.timestamp.format("%H:%M:%S"));
    let arrow = " → ";
    let task = if entry.task_id.is_empty() {
        String::new()
    } else {
        format!(" [{}]", crate::tasks::short_task_id(&entry.task_id))
    };
    let suffix = ": ";
    let prefix_width = time.chars().count()
        + 1
        + entry.from.chars().count()
        + arrow.chars().count()
        + entry.to.chars().count()
        + task.chars().count()
        + suffix.chars().count();
    let prefix_text = format!("{time} {}{arrow}{}{task}{suffix}", entry.from, entry.to);
    if prefix_width >= width {
        let style = selected_style.unwrap_or_else(theme::timestamp);
        return Line::from(Span::styled(
            crate::tasks::truncate(&prefix_text, width),
            style,
        ));
    }

    let body = entry.content.replace(['\n', '\r'], " ");
    let body = crate::tasks::truncate(&body, width - prefix_width);
    let time_style = selected_style.unwrap_or_else(theme::timestamp);
    let from_style = selected_style.unwrap_or_else(theme::sender);
    let arrow_style = selected_style.unwrap_or_else(theme::muted);
    let to_style = selected_style.unwrap_or_else(theme::assignee);
    let task_style = selected_style.unwrap_or_else(theme::task_id);
    let body_style = selected_style.unwrap_or_else(|| message_body_style(&entry.content));

    let mut spans = vec![
        Span::styled(time, time_style),
        Span::styled(" ", arrow_style),
        Span::styled(entry.from.clone(), from_style),
        Span::styled(arrow, arrow_style),
        Span::styled(entry.to.clone(), to_style),
    ];
    if !task.is_empty() {
        spans.push(Span::styled(task, task_style));
    }
    spans.push(Span::styled(suffix, arrow_style));
    spans.push(Span::styled(body, body_style));
    Line::from(spans)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpToolActivity {
    tool: String,
    status: String,
    detail: String,
}

fn mcp_tool_activity(entry: &ax_daemon::HistoryEntry) -> Option<McpToolActivity> {
    let body = entry.content.trim().replace(['\n', '\r'], " ");
    let rest = body.strip_prefix("mcp tool ")?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let status_idx = parts
        .iter()
        .position(|part| matches!(*part, "ok" | "error"))?;
    if status_idx == 0 {
        return None;
    }
    Some(McpToolActivity {
        tool: parts[..status_idx].join(" "),
        status: parts[status_idx].to_owned(),
        detail: parts[status_idx + 1..].join(" "),
    })
}

fn mcp_tool_activity_line(
    entry: &ax_daemon::HistoryEntry,
    activity: &McpToolActivity,
    width: usize,
    selected: bool,
    focused: bool,
) -> Line<'static> {
    let selected_style = selected.then(|| theme::selection(focused));
    let time = format!(" {}", entry.timestamp.format("%H:%M:%S"));
    let task = if entry.task_id.is_empty() {
        String::new()
    } else {
        format!(" [{}]", crate::tasks::short_task_id(&entry.task_id))
    };
    let suffix = ": ";
    let detail = if activity.detail.is_empty() {
        activity.status.clone()
    } else {
        format!("{} {}", activity.status, activity.detail)
    };
    let prefix_text = format!("{time} {} used {}{task}{suffix}", entry.from, activity.tool);
    let prefix_width = prefix_text.chars().count();
    if prefix_width >= width {
        let style = selected_style.unwrap_or_else(theme::timestamp);
        return Line::from(Span::styled(
            crate::tasks::truncate(&prefix_text, width),
            style,
        ));
    }

    let detail = crate::tasks::truncate(&detail, width - prefix_width);
    let time_style = selected_style.unwrap_or_else(theme::timestamp);
    let actor_style = selected_style.unwrap_or_else(theme::sender);
    let verb_style = selected_style.unwrap_or_else(theme::muted);
    let tool_style = selected_style.unwrap_or_else(theme::info);
    let task_style = selected_style.unwrap_or_else(theme::task_id);
    let body_style = selected_style.unwrap_or_else(|| message_body_style(&entry.content));

    let mut spans = vec![
        Span::styled(time, time_style),
        Span::styled(" ", verb_style),
        Span::styled(entry.from.clone(), actor_style),
        Span::styled(" used ", verb_style),
        Span::styled(activity.tool.clone(), tool_style),
    ];
    if !task.is_empty() {
        spans.push(Span::styled(task, task_style));
    }
    spans.push(Span::styled(suffix, verb_style));
    spans.push(Span::styled(detail, body_style));
    Line::from(spans)
}

fn message_body_style(text: &str) -> Style {
    let lower = text.to_ascii_lowercase();
    if lower.contains("error")
        || lower.contains("failed")
        || lower.contains("failure")
        || lower.contains("panic")
    {
        theme::severity(Severity::Danger)
    } else if lower.contains("blocked")
        || lower.contains("blocker")
        || lower.contains("warning")
        || lower.contains("stale")
        || lower.contains("wake")
    {
        theme::severity(Severity::Warning)
    } else if lower.contains("completed")
        || lower.contains("success")
        || lower.contains("done")
        || lower.contains("remaining owned dirty files=<none>")
    {
        theme::severity(Severity::Success)
    } else {
        Style::default()
    }
}

fn token_trend_style(series: &[u64]) -> Style {
    let Some(first) = series.iter().copied().find(|value| *value > 0) else {
        return theme::muted();
    };
    let last = series
        .iter()
        .rev()
        .copied()
        .find(|value| *value > 0)
        .unwrap_or(first);

    match last.cmp(&first) {
        std::cmp::Ordering::Greater => theme::severity(Severity::Warning),
        std::cmp::Ordering::Less => theme::severity(Severity::Success),
        std::cmp::Ordering::Equal => theme::accent(),
    }
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
        f.render_widget(Paragraph::new(hint).style(theme::muted()), inner);
        return;
    }

    let max_total = rows.iter().map(|t| t.total.total()).max().unwrap_or(0) as f64;

    // Caption + column header. One row each, rendered as plain
    // Paragraphs on the top of the inner area so the per-workspace
    // grid below can lean on a real Layout split (needed for the
    // inline Sparkline).
    let compact = inner.width < 96;
    let caption = Paragraph::new(if compact {
        " last 24h · compact token usage"
    } else {
        " last 24h · ▁▂▃▄▅▆▇ = rolling usage per 5-min bucket"
    })
    .style(theme::muted());
    let header_text = if compact {
        format!(" {:<24} {:<9} {:<12}  TREND", "WORKSPACE", "TOTAL", "LAST")
    } else {
        format!(
            " {:<24} {:<14} {:<9} {:<9} {:<9} {:<7} {:<12}  TREND",
            "WORKSPACE", "MODEL", "INPUT", "OUTPUT", "CACHE", "TURNS", "LAST"
        )
    };
    let header = Paragraph::new(header_text).style(theme::muted());

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

    // Tokens uses the same list policy as Messages/Tasks: the cursor
    // selects a workspace row, and the viewport follows to keep that
    // row visible without losing the stable A→Z order.
    let total_rows = rows.len();
    let selected = app.tokens_cursor.index.min(total_rows.saturating_sub(1));
    let viewport = compute_viewport(total_rows, selected, list_area.height as usize);
    let content_area = viewport.content_area(list_area);

    // Text column reserves a fixed width; the sparkline absorbs the
    // rest (with a lower bound so it isn't degenerate on narrow
    // panes).
    let text_width: u16 = if compact { 48 } else { 94 };
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
    for (idx, trend) in rows[viewport.start..viewport.end].iter().enumerate() {
        let absolute = viewport.start + idx;
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
        let selected_style =
            (absolute == selected).then(|| theme::selection(app.focus == Focus::List));
        let style = selected_style.unwrap_or_else(|| {
            if max_total > 0.0 && total >= max_total * 0.8 {
                theme::severity(Severity::Warning)
            } else {
                Style::default()
            }
        });

        let row = if compact {
            format!(
                " {:<24} {:<9} {:<12}",
                crate::tasks::truncate(&trend.workspace, 24),
                crate::tokens::format_token_count(total),
                last,
            )
        } else {
            format!(
                " {:<24} {:<14} {:<9} {:<9} {:<9} {:<7} {:<12}",
                crate::tasks::truncate(&trend.workspace, 24),
                crate::tasks::truncate(&short_model(&model), 14),
                input,
                output,
                cache,
                turns_display,
                last,
            )
        };
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
        let trend_style = token_trend_style(&series);
        if series.iter().any(|v| *v > 0) {
            let spark = Sparkline::default().data(&series).style(trend_style);
            f.render_widget(spark, row_spark);
        } else {
            f.render_widget(Paragraph::new(" (flat)").style(trend_style), row_spark);
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
fn format_last_activity(
    now: chrono::DateTime<chrono::Utc>,
    ts: chrono::DateTime<chrono::Utc>,
) -> String {
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
        let (text, style) = if let Some(error) = &app.task_snapshot_error {
            (
                crate::tasks::truncate(&format!("  snapshot error: {error}"), area.width as usize),
                theme::severity(Severity::Danger),
            )
        } else {
            ("  (no tasks yet)".to_owned(), theme::muted())
        };
        let para = Paragraph::new(text).style(style);
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
    let summary_line = if let Some(error) = &app.task_snapshot_error {
        Line::from(Span::styled(
            crate::tasks::truncate(&format!("snapshot error: {error}"), inner_width.max(1)),
            theme::severity(Severity::Danger),
        ))
    } else {
        format_task_summary_line(&summary, inner_width.max(1))
    };
    let header_lines = vec![
        Line::from(Span::styled(
            crate::tasks::truncate(&title_line, inner_width.max(1)),
            theme::active_label(app.focus == Focus::List),
        )),
        summary_line,
        task_header_line(inner_width.max(1)),
    ];
    let visible_headers: Vec<Line> = header_lines
        .into_iter()
        .take(header_height as usize)
        .collect();
    f.render_widget(Paragraph::new(visible_headers), header_area);

    if filtered.is_empty() {
        if body_area.height > 0 {
            let para = Paragraph::new("  (no tasks match current filter — press f to cycle)")
                .style(theme::muted());
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
        lines.push(format_task_row(
            task,
            rows_width,
            absolute == app.task_cursor.index,
            app.focus == Focus::List,
        ));
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
        let para = Paragraph::new("  (no task selected)").style(theme::muted());
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
    let window: Vec<Line> = all_lines
        .into_iter()
        .skip(scroll)
        .take(end - scroll)
        .collect();
    f.render_widget(Paragraph::new(window), area);
}

/// Agent detail pane. The local tab strip in the title refines the
/// selected workspace context without changing the top-level body tab.
fn draw_agents_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(workspace) = app.selected_workspace().map(ToOwned::to_owned) else {
        let para =
            Paragraph::new("  (select an agent with ↑/↓ to see its detail)").style(theme::muted());
        f.render_widget(para, area);
        return;
    };
    match app.agent_detail_tab {
        AgentDetailTab::Overview => draw_agent_overview_detail(f, area, app, &workspace),
        AgentDetailTab::Tasks => draw_agent_tasks_detail(f, area, app, &workspace),
        AgentDetailTab::Messages => draw_agent_messages_detail(f, area, app, &workspace),
        AgentDetailTab::Instructions => draw_agent_instructions_detail(f, area, app, &workspace),
        AgentDetailTab::Activity => draw_agent_activity_detail(f, area, app, &workspace),
    }
}

/// Overview preserves the pre-tab dashboard: name/status, reconcile
/// notes, live token readings, and the tail of the tmux capture.
fn draw_agent_overview_detail(f: &mut Frame, area: Rect, app: &App, workspace: &str) {
    let info = app.workspace_infos.get(workspace);
    let trend = app.usage_trends.get(workspace);
    let capture = app
        .captures
        .entries
        .get(workspace)
        .map_or("", |e| e.content.as_str());
    let session = app.sessions.iter().find(|s| s.workspace == workspace);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("workspace  ".to_owned(), theme::muted()),
        Span::styled(workspace.to_owned(), theme::strong()),
    ]));
    let status_label = info
        .map(|w| agent_status_str(&w.status).to_owned())
        .unwrap_or_else(|| "offline".to_owned());
    let status_style = info.map_or_else(theme::disabled, |w| theme::agent_status(&w.status));
    lines.push(Line::from(vec![
        Span::styled("status     ".to_owned(), theme::muted()),
        Span::styled(status_label, status_style),
    ]));
    if let Some(info) = info {
        if let Some(git) = &info.git_status {
            let git_state = normalized_git_state(git);
            lines.push(Line::from(vec![
                Span::styled("git        ".to_owned(), theme::muted()),
                Span::styled(format_git_status_detail(git), theme::git_state(&git_state)),
            ]));
        }
        if !info.status_text.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("note       ".to_owned(), theme::muted()),
                Span::raw(info.status_text.clone()),
            ]));
        }
    }
    if let Some(s) = session {
        lines.push(Line::from(vec![
            Span::styled("tmux       ".to_owned(), theme::muted()),
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
            Span::styled("tokens     ".to_owned(), theme::muted()),
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
            theme::muted(),
        )));
        let rows_budget = area
            .height
            .saturating_sub(lines.len() as u16)
            .saturating_sub(1) as usize;
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

fn draw_agent_tasks_detail(f: &mut Frame, area: Rect, app: &App, workspace: &str) {
    let related = agent_related_tasks(&app.tasks, workspace);
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("agent tasks · {workspace}"),
        theme::strong(),
    )));
    push_detail_kv(
        &mut lines,
        "policy",
        "assignee / created_by / claimed_by / task log workspace".to_owned(),
        theme::muted(),
        area.width as usize,
    );
    push_detail_kv(
        &mut lines,
        "source",
        "tasks-state snapshot".to_owned(),
        theme::muted(),
        area.width as usize,
    );
    if let Some(error) = &app.task_snapshot_error {
        push_detail_kv(
            &mut lines,
            "snapshot",
            error.clone(),
            theme::severity(Severity::Danger),
            area.width as usize,
        );
    }
    if related.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("(no tasks for {workspace} in the loaded task snapshot)"),
            theme::muted(),
        )));
        apply_detail_scroll(f, area, app, lines);
        return;
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("{} matching task{}", related.len(), plural(related.len())),
        theme::muted(),
    )));
    lines.push(agent_task_header_line(area.width as usize));
    for task in related {
        lines.push(agent_task_line(task, workspace, area.width as usize));
    }
    apply_detail_scroll(f, area, app, lines);
}

fn draw_agent_messages_detail(f: &mut Frame, area: Rect, app: &mut App, workspace: &str) {
    let related = agent_related_messages(&app.messages, workspace);
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("agent messages · {workspace}"),
        theme::strong(),
    )));
    push_detail_kv(
        &mut lines,
        "policy",
        "message from/to selected workspace".to_owned(),
        theme::muted(),
        area.width as usize,
    );
    push_detail_kv(
        &mut lines,
        "source",
        "message_history loaded tail (max 500 rows)".to_owned(),
        theme::muted(),
        area.width as usize,
    );
    if let Some(error) = &app.messages_snapshot_error {
        push_detail_kv(
            &mut lines,
            "snapshot",
            error.clone(),
            theme::severity(Severity::Danger),
            area.width as usize,
        );
    }
    if related.is_empty() {
        lines.push(Line::from(""));
        let text = if app.messages.is_empty() {
            "(no message history rows loaded yet)"
        } else {
            "(no messages for this workspace in the loaded message tail)"
        };
        lines.push(Line::from(Span::styled(text.to_owned(), theme::muted())));
        apply_tail_detail_scroll(f, area, app, lines);
        return;
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{} matching message{} in loaded tail; older rows may be absent",
            related.len(),
            plural(related.len())
        ),
        theme::muted(),
    )));
    for entry in related {
        lines.push(message_list_line(entry, area.width as usize, false, false));
    }
    apply_tail_detail_scroll(f, area, app, lines);
}

fn draw_agent_instructions_detail(f: &mut Frame, area: Rect, app: &App, workspace: &str) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("agent config · {workspace}"),
        theme::strong(),
    )));
    let Some(mut detail) = find_agent_config_detail(app.tree.as_ref(), workspace) else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "(selected workspace is not present in the loaded config tree)",
            theme::muted(),
        )));
        lines.push(Line::from(Span::styled(
            "exact generated orchestrator prompt display is non-MVP",
            theme::muted(),
        )));
        apply_detail_scroll(f, area, app, lines);
        return;
    };
    if let Some(dir) = app.workspace_dirs.get(workspace) {
        detail.dir = dir.display().to_string();
    }
    push_detail_kv(
        &mut lines,
        "kind",
        detail.kind.clone(),
        theme::info(),
        area.width as usize,
    );
    push_detail_kv(
        &mut lines,
        "project",
        detail.project,
        theme::muted(),
        area.width as usize,
    );
    push_detail_kv(
        &mut lines,
        "dir",
        detail.dir,
        theme::muted(),
        area.width as usize,
    );
    push_detail_kv(
        &mut lines,
        "runtime",
        if detail.runtime.is_empty() {
            "(default)".to_owned()
        } else {
            detail.runtime
        },
        theme::muted(),
        area.width as usize,
    );
    if !detail.description.is_empty() {
        push_detail_kv(
            &mut lines,
            "description",
            detail.description,
            theme::info(),
            area.width as usize,
        );
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "instructions:".to_owned(),
        theme::strong(),
    )));
    if detail.instructions.trim().is_empty() {
        let text = if detail.kind == "orchestrator" {
            "(generated orchestrator prompt is not displayed in the MVP)"
        } else {
            "(no config instructions for this workspace)"
        };
        lines.push(Line::from(Span::styled(text.to_owned(), theme::muted())));
    } else {
        for raw in detail.instructions.trim().lines() {
            lines.push(Line::from(Span::raw(crate::tasks::truncate(
                raw,
                area.width as usize,
            ))));
        }
    }
    apply_detail_scroll(f, area, app, lines);
}

fn draw_agent_activity_detail(f: &mut Frame, area: Rect, app: &mut App, workspace: &str) {
    let activity = agent_mcp_activity_entries(&app.messages, workspace);
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("agent activity · {workspace}"),
        theme::strong(),
    )));
    push_detail_kv(
        &mut lines,
        "policy",
        "MCP tool activity text from message history".to_owned(),
        theme::muted(),
        area.width as usize,
    );
    push_detail_kv(
        &mut lines,
        "source",
        "message_history loaded tail; structured telemetry is non-MVP".to_owned(),
        theme::muted(),
        area.width as usize,
    );
    if activity.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "(no MCP activity for this workspace in the loaded message tail)",
            theme::muted(),
        )));
        apply_tail_detail_scroll(f, area, app, lines);
        return;
    }

    lines.push(Line::from(""));
    for entry in activity {
        lines.push(agent_activity_line(entry, area.width as usize));
    }
    apply_tail_detail_scroll(f, area, app, lines);
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn agent_related_tasks<'a>(
    tasks: &'a [ax_proto::types::Task],
    workspace: &str,
) -> Vec<&'a ax_proto::types::Task> {
    tasks
        .iter()
        .filter(|task| task_related_to_agent(task, workspace))
        .collect()
}

fn task_related_to_agent(task: &ax_proto::types::Task, workspace: &str) -> bool {
    task.assignee == workspace
        || task.created_by == workspace
        || task.claimed_by == workspace
        || task.logs.iter().any(|log| log.workspace == workspace)
}

fn agent_task_roles(task: &ax_proto::types::Task, workspace: &str) -> String {
    let mut roles = Vec::new();
    if task.assignee == workspace {
        roles.push("assignee");
    }
    if task.created_by == workspace {
        roles.push("created");
    }
    if task.claimed_by == workspace {
        roles.push("claimed");
    }
    if task.logs.iter().any(|log| log.workspace == workspace) {
        roles.push("log");
    }
    roles.join("+")
}

fn agent_task_header_line(width: usize) -> Line<'static> {
    let raw = "ID       STATE         RELATION       TITLE";
    Line::from(Span::styled(
        crate::tasks::truncate(raw, width.max(1)),
        theme::column_header(),
    ))
}

fn agent_task_line(task: &ax_proto::types::Task, workspace: &str, width: usize) -> Line<'static> {
    let id = crate::tasks::short_task_id(&task.id);
    let state = format_task_state(task);
    let relation = agent_task_roles(task, workspace);
    let raw = format!(
        "{id:<8} {:<13} {:<14} {}",
        crate::tasks::truncate(&state, 13),
        crate::tasks::truncate(&relation, 14),
        task.title,
    );
    Line::from(vec![Span::styled(
        crate::tasks::truncate(&raw, width.max(1)),
        theme::task_status(&task.status, crate::tasks::task_is_stale(task)),
    )])
}

fn agent_related_messages<'a>(
    messages: &'a [ax_daemon::HistoryEntry],
    workspace: &str,
) -> Vec<&'a ax_daemon::HistoryEntry> {
    messages
        .iter()
        .filter(|entry| entry.from == workspace || entry.to == workspace)
        .collect()
}

fn agent_mcp_activity_entries<'a>(
    messages: &'a [ax_daemon::HistoryEntry],
    workspace: &str,
) -> Vec<&'a ax_daemon::HistoryEntry> {
    messages
        .iter()
        .filter(|entry| {
            entry.from == workspace
                && (entry.to == "ax.daemon"
                    || entry.content.to_ascii_lowercase().starts_with("mcp tool "))
        })
        .collect()
}

fn agent_activity_line(entry: &ax_daemon::HistoryEntry, width: usize) -> Line<'static> {
    if let Some(activity) = mcp_tool_activity(entry) {
        return mcp_tool_activity_line(entry, &activity, width, false, false);
    }

    let task = if entry.task_id.is_empty() {
        String::new()
    } else {
        format!(" [{}]", crate::tasks::short_task_id(&entry.task_id))
    };
    let raw = format!(
        " {}{task}: {}",
        entry.timestamp.format("%H:%M:%S"),
        entry.content.replace(['\n', '\r'], " "),
    );
    Line::from(Span::styled(
        crate::tasks::truncate(&raw, width.max(1)),
        message_body_style(&entry.content),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentConfigDetail {
    kind: String,
    project: String,
    dir: String,
    runtime: String,
    description: String,
    instructions: String,
}

fn find_agent_config_detail(
    tree: Option<&ProjectNode>,
    workspace: &str,
) -> Option<AgentConfigDetail> {
    find_agent_config_in_node(tree?, workspace)
}

fn find_agent_config_in_node(node: &ProjectNode, workspace: &str) -> Option<AgentConfigDetail> {
    let orchestrator = if node.prefix.is_empty() {
        "orchestrator".to_owned()
    } else {
        format!("{}.orchestrator", node.prefix)
    };
    if !node.disable_root_orchestrator && workspace == orchestrator {
        return Some(AgentConfigDetail {
            kind: "orchestrator".to_owned(),
            project: node.display_name(),
            dir: node.dir.display().to_string(),
            runtime: node.orchestrator_runtime.clone(),
            description: String::new(),
            instructions: String::new(),
        });
    }

    for ws in &node.workspaces {
        if ws.merged_name == workspace {
            return Some(AgentConfigDetail {
                kind: "workspace".to_owned(),
                project: node.display_name(),
                dir: node.dir.display().to_string(),
                runtime: ws.runtime.clone(),
                description: ws.description.clone(),
                instructions: ws.instructions.clone(),
            });
        }
    }

    node.children
        .iter()
        .find_map(|child| find_agent_config_in_node(child, workspace))
}

/// Messages detail. Shows the full content of the message at the
/// current scroll tail so operators can read wrapped long messages
/// without leaving the tab. When no messages exist, renders a dim
/// placeholder so the pane isn't suspiciously empty.
fn draw_messages_detail(f: &mut Frame, area: Rect, app: &App) {
    if app.messages.is_empty() {
        let para = Paragraph::new("  (no messages yet)").style(theme::muted());
        f.render_widget(para, area);
        return;
    }
    // `messages_cursor.index` is an absolute row — the same row the
    // list pane highlights — so detail naturally tracks the selection.
    let idx = app
        .messages_cursor
        .index
        .min(app.messages.len().saturating_sub(1));
    let entry = &app.messages[idx];

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("time   ".to_owned(), theme::muted()),
        Span::styled(
            entry.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            theme::timestamp(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("from   ".to_owned(), theme::muted()),
        Span::styled(entry.from.clone(), theme::sender()),
        Span::styled("  →  ", theme::muted()),
        Span::styled(entry.to.clone(), theme::assignee()),
    ]));
    if !entry.task_id.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("task   ".to_owned(), theme::muted()),
            Span::styled(entry.task_id.clone(), theme::task_id()),
        ]));
    }
    lines.push(Line::from(""));
    for raw in entry.content.lines() {
        lines.push(Line::from(Span::styled(
            raw.to_owned(),
            message_body_style(raw),
        )));
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
        let para = Paragraph::new("  (no token activity yet)").style(theme::muted());
        f.render_widget(para, area);
        return;
    }
    rows.sort_by(|a, b| a.workspace.cmp(&b.workspace));
    let idx = app.tokens_cursor.index.min(rows.len().saturating_sub(1));
    let t = rows[idx];

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("workspace  ".to_owned(), theme::muted()),
        Span::styled(t.workspace.clone(), theme::strong()),
    ]));
    if !t.latest_model.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("model      ".to_owned(), theme::muted()),
            Span::raw(short_model(&t.latest_model)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("input      ".to_owned(), theme::muted()),
        Span::raw(crate::tokens::format_token_count(t.total.input as f64)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("output     ".to_owned(), theme::muted()),
        Span::raw(crate::tokens::format_token_count(t.total.output as f64)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("cache read ".to_owned(), theme::muted()),
        Span::raw(crate::tokens::format_token_count(t.total.cache_read as f64)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("cache creat".to_owned(), theme::muted()),
        Span::raw(crate::tokens::format_token_count(
            t.total.cache_creation as f64,
        )),
    ]));
    if let Some(last) = t.last_activity {
        lines.push(Line::from(vec![
            Span::styled("last active".to_owned(), theme::muted()),
            Span::raw(format_last_activity(chrono::Utc::now(), last)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("buckets    ".to_owned(), theme::muted()),
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
        .style(theme::muted());
        f.render_widget(para, area);
        return;
    };
    let info = app.workspace_infos.get(&workspace);
    let session = app.sessions.iter().find(|s| s.workspace == workspace);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("mirroring  ".to_owned(), theme::muted()),
        Span::styled(workspace.clone(), theme::strong()),
    ]));
    if let Some(info) = info {
        lines.push(Line::from(vec![
            Span::styled("status     ".to_owned(), theme::muted()),
            Span::styled(
                agent_status_str(&info.status).to_owned(),
                theme::agent_status(&info.status),
            ),
        ]));
        if !info.status_text.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("note       ".to_owned(), theme::muted()),
                Span::raw(info.status_text.clone()),
            ]));
        }
    }
    if let Some(s) = session {
        lines.push(Line::from(vec![
            Span::styled("session    ".to_owned(), theme::muted()),
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

fn apply_tail_detail_scroll(f: &mut Frame, area: Rect, app: &mut App, lines: Vec<Line<'static>>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let total = lines.len();
    let visible = (area.height as usize).min(total);
    let max_scroll = total.saturating_sub(visible);
    if app.agent_detail_follow_tail {
        app.detail_scroll.index = max_scroll;
    } else {
        app.detail_scroll.clamp(max_scroll);
        if app.detail_scroll.index == max_scroll {
            app.agent_detail_follow_tail = true;
        }
    }
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
            .position(self.scrollbar_position())
            .viewport_content_length(self.visible)
    }

    fn scrollbar_position(self) -> usize {
        if self.total == 0 || self.visible == 0 {
            return 0;
        }
        let max_start = self.total.saturating_sub(self.visible);
        if max_start == 0 {
            return 0;
        }
        let start = self.start.min(max_start);
        let max_position = self.total.saturating_sub(1);
        start
            .saturating_mul(max_position)
            .saturating_add(max_start / 2)
            / max_start
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

#[cfg(test)]
fn viewport_scrollbar_position(total: usize, selected: usize, budget: usize) -> usize {
    compute_viewport(total, selected, budget).scrollbar_position()
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

fn format_task_row(
    task: &ax_proto::types::Task,
    width: usize,
    selected: bool,
    focused: bool,
) -> Line<'static> {
    let id = crate::tasks::short_task_id(&task.id);
    let state = format_task_state(task);
    let selected_style = selected.then(|| theme::selection(focused));
    let id_style = selected_style.unwrap_or_else(theme::task_id);
    let state_style = selected_style
        .unwrap_or_else(|| theme::task_status(&task.status, crate::tasks::task_is_stale(task)));
    let assignee_style = selected_style.unwrap_or_else(theme::assignee);
    let title_style = selected_style.unwrap_or_else(|| theme::task_title(&task.status));

    let fixed = 8 + 1 + 13 + 1 + 12 + 1;
    if width <= fixed {
        let row = format!(
            "{id:<8} {:<13} {:<12} {}",
            crate::tasks::truncate(&state, 13),
            crate::tasks::truncate(&task.assignee, 12),
            task.title,
        );
        let style = selected_style
            .unwrap_or_else(|| theme::task_status(&task.status, crate::tasks::task_is_stale(task)));
        return Line::from(Span::styled(
            crate::tasks::truncate(&row, width.max(1)),
            style,
        ));
    }

    let mut spans = Vec::new();
    let gap_style = selected_style.unwrap_or_else(Style::default);
    push_padded_span(&mut spans, &id, 8, id_style);
    push_gap_with(&mut spans, gap_style);
    push_padded_span(&mut spans, &state, 13, state_style);
    push_gap_with(&mut spans, gap_style);
    push_padded_span(&mut spans, &task.assignee, 12, assignee_style);
    push_gap_with(&mut spans, gap_style);
    push_padded_span(&mut spans, &task.title, width - fixed, title_style);
    Line::from(spans)
}

fn task_header_line(width: usize) -> Line<'static> {
    let fixed = 8 + 1 + 13 + 1 + 12 + 1;
    if width <= fixed {
        return Line::from(Span::styled(
            crate::tasks::truncate("ID       STATE         OWNER        TITLE", width),
            theme::column_header(),
        ));
    }
    let mut spans = Vec::new();
    push_padded_span(&mut spans, "ID", 8, theme::column_header());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "STATE", 13, theme::column_header());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "OWNER", 12, theme::column_header());
    push_gap(&mut spans);
    push_padded_span(&mut spans, "TITLE", width - fixed, theme::column_header());
    Line::from(spans)
}

fn format_task_summary_line(summary: &crate::tasks::TaskSummary, width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let parts = [
        (format!("tot {}", summary.total), theme::strong()),
        (
            format!("run {}", summary.in_progress),
            theme::task_status(&ax_proto::types::TaskStatus::InProgress, false),
        ),
        (
            format!("pend {}", summary.pending),
            theme::task_status(&ax_proto::types::TaskStatus::Pending, false),
        ),
        (
            format!("stale {}", summary.stale),
            theme::severity(Severity::Warning),
        ),
        (
            format!("block {}", summary.blocked),
            theme::task_status(&ax_proto::types::TaskStatus::Blocked, false),
        ),
        (
            format!("fail {}", summary.failed),
            theme::task_status(&ax_proto::types::TaskStatus::Failed, false),
        ),
        (
            format!("done {}", summary.completed),
            theme::task_status(&ax_proto::types::TaskStatus::Completed, false),
        ),
        (format!("msg {}", summary.queued_messages), theme::task_id()),
        (
            format!("div {}", summary.diverged),
            theme::severity(Severity::Warning),
        ),
        (
            format!("hi {}", summary.urgent_or_high),
            theme::priority(Some(&ax_proto::types::TaskPriority::High)),
        ),
        (
            format!("cancel {}", summary.cancelled),
            theme::task_status(&ax_proto::types::TaskStatus::Cancelled, false),
        ),
    ];

    let mut used = 0usize;
    for (text, style) in parts {
        let is_optional_zero = text.ends_with(" 0")
            && !text.starts_with("tot ")
            && !text.starts_with("run ")
            && !text.starts_with("pend ")
            && !text.starts_with("stale ");
        if is_optional_zero {
            continue;
        }
        let sep = if spans.is_empty() { "" } else { " · " };
        let needed = sep.chars().count() + text.chars().count();
        if used + needed > width {
            break;
        }
        if !sep.is_empty() {
            spans.push(Span::styled(sep.to_owned(), theme::muted()));
        }
        spans.push(Span::styled(text, style));
        used += needed;
    }
    if spans.is_empty() {
        return Line::from(Span::styled(
            crate::tasks::truncate(&format_task_summary_compact(summary), width),
            theme::strong(),
        ));
    }
    Line::from(spans)
}

fn format_task_summary_compact(summary: &crate::tasks::TaskSummary) -> String {
    let mut parts = vec![
        format!("tot {}", summary.total),
        format!("run {}", summary.in_progress),
        format!("pend {}", summary.pending),
        format!("stale {}", summary.stale),
    ];
    if summary.blocked > 0 {
        parts.push(format!("block {}", summary.blocked));
    }
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

fn push_detail_kv<'a>(
    out: &mut Vec<Line<'a>>,
    label: &'static str,
    value: String,
    value_style: Style,
    width: usize,
) {
    let prefix = format!("{label}: ");
    let prefix_width = prefix.chars().count();
    let value_width = width.max(1).saturating_sub(prefix_width);
    out.push(Line::from(vec![
        Span::styled(prefix, theme::meta_label()),
        Span::styled(
            crate::tasks::truncate(&value, value_width.max(1)),
            value_style,
        ),
    ]));
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

    out.push(Line::from(Span::styled(
        truncate(&task.title, width.max(1)),
        theme::task_title(&task.status).add_modifier(ratatui::style::Modifier::BOLD),
    )));
    out.push(Line::from(Span::styled(
        truncate(
            &format!("status: {}", task_status_label(task)),
            width.max(1),
        ),
        theme::task_status(&task.status, task_is_stale(task)),
    )));
    push_detail_kv(
        &mut out,
        "version",
        task.version.to_string(),
        theme::muted(),
        width,
    );
    push_detail_kv(
        &mut out,
        "assignee",
        task.assignee.clone(),
        theme::assignee(),
        width,
    );
    push_detail_kv(
        &mut out,
        "created_by",
        task.created_by.clone(),
        theme::sender(),
        width,
    );
    push_detail_kv(
        &mut out,
        "priority",
        task_priority_label(task.priority.as_ref()).to_owned(),
        theme::priority(task.priority.as_ref()),
        width,
    );
    push_detail_kv(
        &mut out,
        "updated",
        format!("{} ago", format_task_age(task)),
        theme::timestamp(),
        width,
    );
    push_detail_kv(
        &mut out,
        "stale",
        stale_flag.to_owned(),
        if stale_flag == "yes" {
            theme::severity(Severity::Warning)
        } else {
            theme::muted()
        },
        width,
    );
    if task.stale_after_seconds > 0 {
        push_detail_kv(
            &mut out,
            "stale_after",
            format!("{}s", task.stale_after_seconds),
            theme::timestamp(),
            width,
        );
    }
    if let Some(ts) = task.removed_at {
        push_detail_kv(
            &mut out,
            "removed",
            ts.format("%Y-%m-%d %H:%M:%S").to_string(),
            theme::timestamp(),
            width,
        );
        if !task.removed_by.is_empty() {
            push_detail_kv(
                &mut out,
                "removed_by",
                task.removed_by.clone(),
                theme::sender(),
                width,
            );
        }
    }
    if !task.description.is_empty() {
        out.push(Line::from(""));
        push_detail_kv(
            &mut out,
            "desc",
            task.description.clone(),
            Style::default(),
            width,
        );
    }
    if !task.result.is_empty() {
        out.push(Line::from(""));
        push_detail_kv(
            &mut out,
            "result",
            task.result.clone(),
            theme::task_status(&task.status, false),
            width,
        );
    }
    if let Some(info) = &task.stale_info {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(
            "stale_info:".to_owned(),
            theme::severity_bold(Severity::Warning),
        )));
        if !info.reason.is_empty() {
            push_detail_kv(
                &mut out,
                "  reason",
                info.reason.clone(),
                theme::severity(Severity::Warning),
                width,
            );
        }
        if !info.recommended_action.is_empty() {
            push_detail_kv(
                &mut out,
                "  action",
                info.recommended_action.clone(),
                theme::info(),
                width,
            );
        }
        if info.pending_messages > 0 {
            push_detail_kv(
                &mut out,
                "  pending_messages",
                info.pending_messages.to_string(),
                theme::task_id(),
                width,
            );
        }
        if info.wake_pending {
            push_detail_kv(
                &mut out,
                "  wake_attempts",
                info.wake_attempts.to_string(),
                theme::severity(Severity::Warning),
                width,
            );
        }
        if info.state_divergence {
            push_detail_kv(
                &mut out,
                "  divergence",
                info.state_divergence_note.clone(),
                theme::severity(Severity::Warning),
                width,
            );
        }
    }

    let logs: Vec<_> = task.logs.iter().rev().take(3).collect();
    if !logs.is_empty() {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(
            "recent logs:".to_owned(),
            theme::strong(),
        )));
        for log in logs.into_iter().rev() {
            out.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", log.timestamp.format("%H:%M:%S")),
                    theme::timestamp(),
                ),
                Span::styled(log.workspace.clone(), theme::assignee()),
                Span::styled(": ", theme::muted()),
                Span::styled(
                    crate::tasks::truncate(&log.message, width.saturating_sub(13).max(1)),
                    message_body_style(&log.message),
                ),
            ]));
        }
    }

    let activity = crate::tasks::build_task_activity(task, history, 4);
    if !activity.is_empty() {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(
            "activity:".to_owned(),
            theme::strong(),
        )));
        for entry in &activity {
            out.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", entry.timestamp.format("%H:%M:%S")),
                    theme::timestamp(),
                ),
                Span::styled(format!("{:<9} ", entry.kind.label()), theme::info()),
                Span::styled(
                    crate::tasks::truncate(&entry.summary, width.saturating_sub(20).max(1)),
                    message_body_style(&entry.summary),
                ),
            ]));
        }
    }

    if out.len() > height {
        out.truncate(height);
    }
    out
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let (text, style) = if let Some(notice) = &app.quick_notice {
        (notice.text.clone(), theme::notice(notice.error))
    } else if let Some(msg) = &app.notice {
        (msg.text.clone(), theme::muted())
    } else if app.quick_actions.open {
        (
            "↑↓ action · enter run · esc close · q quit".to_owned(),
            theme::muted(),
        )
    } else {
        (focus_footer_hint(app), theme::muted())
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
    let numeric_range = if app.streamed_workspace.is_some() {
        "1-5"
    } else {
        "1-4"
    };
    let base = format!("[/] pane · Tab/{numeric_range} view · f filter · ? help · q quit");
    let scoped = match app.focus {
        Focus::List => match app.stream {
            StreamView::Agents => "↑↓/jk agent · enter actions",
            StreamView::Tasks => "↑↓/jk task",
            StreamView::Messages | StreamView::Tokens => "↑↓/jk scroll · g/G head/tail",
            StreamView::Stream => {
                if app.stream_follow_tail {
                    "↑↓/jk freeze scroll · g/G top/follow"
                } else {
                    "↑↓/jk scroll · G follow tail"
                }
            }
        },
        Focus::Detail => {
            if app.stream == StreamView::Agents {
                if matches!(
                    app.agent_detail_tab,
                    AgentDetailTab::Messages | AgentDetailTab::Activity
                ) {
                    "h/l detail tab · ↑↓/jk scroll · g/G head/tail · esc list"
                } else {
                    "h/l detail tab · ↑↓/jk scroll · g reset · esc list"
                }
            } else {
                "↑↓/jk scroll · g reset · esc list"
            }
        }
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
    use ax_proto::types::{Task, TaskStartMode, TaskStatus, WorkspaceInfo};
    use ax_proto::usage::{Tokens, UsageBucket, WorkspaceTrend};
    use ratatui::style::{Color, Modifier};

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
    fn viewport_scrollbar_position_maps_bottom_window_to_track_end() {
        assert_eq!(viewport_scrollbar_position(20, 0, 5), 0);
        assert_eq!(viewport_scrollbar_position(20, 4, 5), 0);
        assert_eq!(viewport_scrollbar_position(20, 19, 5), 19);
    }

    #[test]
    fn compact_task_summary_prioritizes_short_labels() {
        let summary = crate::tasks::TaskSummary {
            total: 12,
            pending: 3,
            in_progress: 2,
            completed: 4,
            failed: 1,
            blocked: 1,
            stale: 2,
            diverged: 1,
            queued_messages: 9,
            urgent_or_high: 2,
            ..crate::tasks::TaskSummary::default()
        };
        assert_eq!(
            format_task_summary_compact(&summary),
            "tot 12 · run 2 · pend 3 · stale 2 · block 1 · fail 1 · done 4 · msg 9 · div 1 · hi 2"
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

    #[test]
    fn footer_hint_only_advertises_stream_shortcut_when_pinned() {
        let mut app = App::new();
        let hint = focus_footer_hint(&app);
        assert!(hint.contains("Tab/1-4 view"));
        assert!(!hint.contains("Tab/1-5 view"));

        app.streamed_workspace = Some("alpha".into());
        let hint = focus_footer_hint(&app);
        assert!(hint.contains("Tab/1-5 view"));
    }

    #[test]
    fn footer_hint_advertises_agent_detail_tabs_only_in_agents_detail() {
        let mut app = App::new();
        app.focus = Focus::Detail;
        app.stream = StreamView::Agents;
        assert!(focus_footer_hint(&app).contains("h/l detail tab"));

        app.stream = StreamView::Tasks;
        assert!(!focus_footer_hint(&app).contains("h/l detail tab"));
    }

    #[test]
    fn agent_detail_title_marks_local_tab_context() {
        let mut app = App::new();
        app.agent_entries = vec![AgentEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: Some(0),
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        app.selected_entry = 0;
        app.focus = Focus::Detail;
        app.agent_detail_tab = AgentDetailTab::Messages;

        let line = detail_title(&app, 120);
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.contains("agent detail"));
        assert!(rendered.contains("alpha"));
        assert!(rendered.contains("messages"));
    }

    #[test]
    fn agent_task_filter_covers_safe_relation_fields() {
        let mut assigned = mock_task();
        assigned.id = "assigned".into();
        assigned.assignee = "alpha".into();

        let mut created = mock_task();
        created.id = "created".into();
        created.assignee = "beta".into();
        created.created_by = "alpha".into();

        let mut claimed = mock_task();
        claimed.id = "claimed".into();
        claimed.assignee = "beta".into();
        claimed.created_by = "orch".into();
        claimed.claimed_by = "alpha".into();

        let mut logged = mock_task();
        logged.id = "logged".into();
        logged.assignee = "beta".into();
        logged.created_by = "orch".into();
        logged.logs.push(ax_proto::types::TaskLog {
            timestamp: chrono::Utc::now(),
            workspace: "alpha".into(),
            message: "progress".into(),
        });

        let mut other = mock_task();
        other.id = "other".into();
        other.assignee = "beta".into();
        other.created_by = "orch".into();

        let tasks = vec![assigned, created, claimed, logged, other];
        let ids: Vec<&str> = agent_related_tasks(&tasks, "alpha")
            .into_iter()
            .map(|task| task.id.as_str())
            .collect();
        assert_eq!(ids, vec!["assigned", "created", "claimed", "logged"]);
    }

    #[test]
    fn agent_message_and_activity_filters_use_loaded_history_tail() {
        let base = chrono::Utc::now();
        let messages = vec![
            ax_daemon::HistoryEntry {
                timestamp: base,
                from: "alpha".into(),
                to: "orch".into(),
                content: "hello".into(),
                task_id: String::new(),
            },
            ax_daemon::HistoryEntry {
                timestamp: base,
                from: "beta".into(),
                to: "alpha".into(),
                content: "reply".into(),
                task_id: String::new(),
            },
            ax_daemon::HistoryEntry {
                timestamp: base,
                from: "alpha".into(),
                to: "ax.daemon".into(),
                content: "mcp tool list_tasks ok duration_ms=3".into(),
                task_id: String::new(),
            },
            ax_daemon::HistoryEntry {
                timestamp: base,
                from: "beta".into(),
                to: "ax.daemon".into(),
                content: "mcp tool read_messages ok".into(),
                task_id: String::new(),
            },
        ];

        assert_eq!(agent_related_messages(&messages, "alpha").len(), 3);
        let activity = agent_mcp_activity_entries(&messages, "alpha");
        assert_eq!(activity.len(), 1);
        assert!(activity[0].content.contains("list_tasks"));

        let rendered = span_text(&agent_activity_line(activity[0], 96).spans);
        assert!(rendered.contains("alpha used list_tasks: ok duration_ms=3"));
        assert!(!rendered.contains("alpha → ax.daemon"));
    }

    #[test]
    fn agent_config_lookup_reads_workspace_and_orchestrator_config_fields() {
        let tree = ax_config::ProjectNode {
            name: "root".into(),
            alias: String::new(),
            prefix: String::new(),
            dir: std::path::PathBuf::from("/repo"),
            orchestrator_runtime: "codex".into(),
            disable_root_orchestrator: false,
            workspaces: vec![ax_config::WorkspaceRef {
                name: "cli".into(),
                merged_name: "ax.cli".into(),
                runtime: "codex".into(),
                description: "CLI owner".into(),
                instructions: "Own ax-tui".into(),
            }],
            children: Vec::new(),
        };

        let workspace = find_agent_config_detail(Some(&tree), "ax.cli").expect("workspace config");
        assert_eq!(workspace.kind, "workspace");
        assert_eq!(workspace.description, "CLI owner");
        assert_eq!(workspace.instructions, "Own ax-tui");

        let orchestrator =
            find_agent_config_detail(Some(&tree), "orchestrator").expect("orchestrator config");
        assert_eq!(orchestrator.kind, "orchestrator");
        assert_eq!(orchestrator.runtime, "codex");
        assert!(orchestrator.instructions.is_empty());
    }

    #[test]
    fn draw_renders_selected_agent_local_tab_content() {
        let mut app = App::new();
        app.agent_entries = vec![AgentEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: Some(0),
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        app.selected_entry = 0;
        app.focus = Focus::Detail;
        app.agent_detail_tab = AgentDetailTab::Messages;
        app.messages = vec![ax_daemon::HistoryEntry {
            timestamp: chrono::Utc::now(),
            from: "alpha".into(),
            to: "orchestrator".into(),
            content: "hello from alpha".into(),
            task_id: String::new(),
        }];

        let rendered = render_app_to_string(&mut app, 120, 34);
        assert!(rendered.contains("agent messages"));
        assert!(rendered.contains("hello from alpha"));

        app.agent_detail_tab = AgentDetailTab::Instructions;
        app.tree = Some(ax_config::ProjectNode {
            name: "root".into(),
            alias: String::new(),
            prefix: String::new(),
            dir: std::path::PathBuf::from("/repo"),
            orchestrator_runtime: String::new(),
            disable_root_orchestrator: false,
            workspaces: vec![ax_config::WorkspaceRef {
                name: "alpha".into(),
                merged_name: "alpha".into(),
                runtime: "codex".into(),
                description: "Alpha worker".into(),
                instructions: "Handle alpha tasks".into(),
            }],
            children: Vec::new(),
        });

        let rendered = render_app_to_string(&mut app, 120, 34);
        assert!(rendered.contains("agent config"));
        assert!(rendered.contains("Handle alpha tasks"));
    }

    #[test]
    fn agents_panel_scrollbar_reaches_bottom_when_last_agent_visible() {
        let mut app = App::new();
        app.stream = StreamView::Agents;
        app.focus = Focus::List;
        app.agent_entries = (0..12)
            .map(|idx| AgentEntry {
                label: format!("agent-{idx:02}"),
                workspace: format!("agent-{idx:02}"),
                session_index: Some(idx),
                level: 0,
                group: false,
                reconcile: String::new(),
            })
            .collect();
        app.selected_entry = app.agent_entries.len() - 1;

        let width = 100;
        let height = 20;
        let buffer = render_app_to_buffer(&mut app, width, height);
        let body_area = Rect::new(0, 1, width, height - 2);
        let (pane_area, _) = split_body_inner(body_area);
        let rows_area = Rect::new(
            pane_area.x,
            pane_area.y + 1,
            pane_area.width,
            pane_area.height.saturating_sub(1),
        );
        let scrollbar_x = rows_area.right().saturating_sub(1);
        let bottom_y = rows_area.bottom().saturating_sub(1);

        let (_, selected_y) =
            first_cell_for(&buffer, width, height, "agent-11").expect("last agent visible");
        assert_eq!(
            selected_y, bottom_y,
            "selected last agent should sit on the final visible row"
        );
        assert_eq!(
            buffer[(scrollbar_x, bottom_y)].symbol(),
            symbols::block::FULL,
            "bottom-scrolled agents scrollbar thumb reaches the track end"
        );
    }

    #[test]
    fn draw_tail_follows_agent_message_detail_until_user_scrolls_away() {
        let mut app = App::new();
        app.agent_entries = vec![AgentEntry {
            label: "alpha".into(),
            workspace: "alpha".into(),
            session_index: Some(0),
            level: 0,
            group: false,
            reconcile: String::new(),
        }];
        app.selected_entry = 0;
        app.focus = Focus::Detail;
        app.agent_detail_tab = AgentDetailTab::Messages;
        app.messages = (0..16)
            .map(|idx| ax_daemon::HistoryEntry {
                timestamp: chrono::Utc::now() + chrono::Duration::seconds(idx),
                from: "alpha".into(),
                to: "orchestrator".into(),
                content: format!("message {idx:02}"),
                task_id: String::new(),
            })
            .collect();

        let rendered = render_app_to_string(&mut app, 120, 18);
        assert!(app.agent_detail_follow_tail);
        assert!(app.detail_scroll.index > 0, "initial render snaps to tail");
        assert!(rendered.contains("message 15"));
        assert!(!rendered.contains("message 00"));

        let parked = app.detail_scroll.index.saturating_sub(2);
        app.agent_detail_follow_tail = false;
        app.detail_scroll.index = parked;
        app.messages.push(ax_daemon::HistoryEntry {
            timestamp: chrono::Utc::now() + chrono::Duration::seconds(99),
            from: "alpha".into(),
            to: "orchestrator".into(),
            content: "message 99".into(),
            task_id: String::new(),
        });

        let _ = render_app_to_string(&mut app, 120, 18);
        assert_eq!(
            app.detail_scroll.index, parked,
            "parked local message detail does not auto-steal the cursor"
        );
        assert!(!app.agent_detail_follow_tail);
    }

    #[test]
    fn tokens_panel_highlights_selected_usage_row() {
        let mut app = App::new();
        app.stream = StreamView::Tokens;
        app.focus = Focus::List;
        app.usage_trends
            .insert("alpha".into(), token_trend("alpha", 10));
        app.usage_trends
            .insert("beta".into(), token_trend("beta", 20));
        app.tokens_cursor.index = 1;

        let buffer = render_app_to_buffer(&mut app, 120, 24);
        let (x, y) = first_cell_for(&buffer, 120, 24, "beta").expect("selected row visible");
        assert!(
            buffer[(x, y)].modifier.contains(Modifier::REVERSED),
            "selected token row is highlighted"
        );
        let (trend_x, _) =
            first_cell_for_on_row(&buffer, 120, y, "(flat)").expect("selected trend cell visible");
        assert!(
            !buffer[(trend_x, y)].modifier.contains(Modifier::REVERSED),
            "selected token trend stays foreground-only"
        );
    }

    #[test]
    fn tokens_panel_scrolls_selected_usage_row_into_view() {
        let mut app = App::new();
        app.stream = StreamView::Tokens;
        app.focus = Focus::List;
        for name in [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
        ] {
            app.usage_trends.insert(name.into(), token_trend(name, 10));
        }
        app.tokens_cursor.index = 7;

        let buffer = render_app_to_buffer(&mut app, 96, 14);
        let (x, y) = first_cell_for(&buffer, 96, 14, "hotel").expect("selected row visible");
        assert!(
            buffer[(x, y)].modifier.contains(Modifier::REVERSED),
            "viewport follows the selected token row"
        );
    }

    #[test]
    fn tokens_panel_selected_sparkline_uses_foreground_only_style() {
        let mut app = App::new();
        app.stream = StreamView::Tokens;
        app.focus = Focus::List;
        app.usage_trends.insert(
            "alpha".into(),
            token_trend_with_series("alpha", &[1, 4, 2, 8]),
        );
        app.tokens_cursor.index = 0;

        let buffer = render_app_to_buffer(&mut app, 120, 24);
        let (_, y) = first_cell_for(&buffer, 120, 24, "alpha").expect("selected row visible");
        let (spark_x, _) =
            first_non_space_after(&buffer, 120, y, 92).expect("sparkline cell visible");
        assert_eq!(
            buffer[(spark_x, y)].bg,
            Color::Reset,
            "trend does not use background-heavy styling"
        );
        assert!(
            !buffer[(spark_x, y)].modifier.contains(Modifier::REVERSED),
            "selected sparkline is not inverse/reversed"
        );
        if theme::colors_enabled() {
            assert_ne!(
                buffer[(spark_x, y)].fg,
                Color::Reset,
                "colored terminals get foreground trend color"
            );
        }
    }

    #[test]
    fn token_trend_style_uses_plain_foreground_semantics() {
        let rising = token_trend_style(&[1, 2, 4]);
        let falling = token_trend_style(&[4, 2, 1]);
        let neutral = token_trend_style(&[3, 3, 3]);
        let flat = token_trend_style(&[]);

        for style in [rising, falling, neutral, flat] {
            assert!(style.bg.is_none());
            assert!(!style.add_modifier.contains(Modifier::REVERSED));
        }
        if theme::colors_enabled() {
            assert_ne!(rising.fg, falling.fg);
            assert_ne!(rising.fg, neutral.fg);
        }
    }

    #[test]
    fn group_git_summary_rolls_up_identical_child_statuses() {
        let git = WorkspaceGitStatus {
            state: "dirty".into(),
            modified: 2,
            untracked: 1,
            ..WorkspaceGitStatus::default()
        };
        let mut app = App::new();
        app.agent_entries = vec![
            AgentEntry {
                label: "▾ ax".into(),
                workspace: String::new(),
                session_index: None,
                level: 0,
                group: true,
                reconcile: String::new(),
            },
            AgentEntry {
                label: "orchestrator".into(),
                workspace: "orchestrator".into(),
                session_index: Some(0),
                level: 1,
                group: false,
                reconcile: String::new(),
            },
            AgentEntry {
                label: "cli".into(),
                workspace: "cli".into(),
                session_index: Some(1),
                level: 1,
                group: false,
                reconcile: String::new(),
            },
        ];
        app.workspace_infos.insert(
            "orchestrator".into(),
            workspace_info("orchestrator", git.clone()),
        );
        app.workspace_infos
            .insert("cli".into(), workspace_info("cli", git));

        let (summary, _) = group_git_summary(0, &app, true).expect("group git summary");
        assert_eq!(summary, "changed:2 ?1");
    }

    #[test]
    fn agents_panel_places_group_git_summary_next_to_name_once() {
        let git = WorkspaceGitStatus {
            state: "dirty".into(),
            modified: 2,
            untracked: 1,
            ..WorkspaceGitStatus::default()
        };
        let mut app = App::new();
        app.agent_entries = vec![
            AgentEntry {
                label: "▾ ax".into(),
                workspace: String::new(),
                session_index: None,
                level: 0,
                group: true,
                reconcile: String::new(),
            },
            AgentEntry {
                label: "orchestrator".into(),
                workspace: "orchestrator".into(),
                session_index: Some(0),
                level: 1,
                group: false,
                reconcile: String::new(),
            },
            AgentEntry {
                label: "cli".into(),
                workspace: "cli".into(),
                session_index: Some(1),
                level: 1,
                group: false,
                reconcile: String::new(),
            },
        ];
        app.workspace_infos.insert(
            "orchestrator".into(),
            workspace_info("orchestrator", git.clone()),
        );
        app.workspace_infos
            .insert("cli".into(), workspace_info("cli", git));

        let rendered = render_app_to_string(&mut app, 120, 18);
        assert!(rendered.contains("▾ ax git changed:2 ?1"));
        assert_eq!(rendered.matches("git changed:2 ?1").count(), 1);
    }

    #[test]
    fn group_git_summary_marks_mixed_child_statuses() {
        let dirty = WorkspaceGitStatus {
            state: "dirty".into(),
            modified: 2,
            untracked: 1,
            ..WorkspaceGitStatus::default()
        };
        let clean = WorkspaceGitStatus {
            state: "clean".into(),
            ..WorkspaceGitStatus::default()
        };
        let mut app = App::new();
        app.agent_entries = vec![
            AgentEntry {
                label: "▾ ax".into(),
                workspace: String::new(),
                session_index: None,
                level: 0,
                group: true,
                reconcile: String::new(),
            },
            AgentEntry {
                label: "orchestrator".into(),
                workspace: "orchestrator".into(),
                session_index: Some(0),
                level: 1,
                group: false,
                reconcile: String::new(),
            },
            AgentEntry {
                label: "cli".into(),
                workspace: "cli".into(),
                session_index: Some(1),
                level: 1,
                group: false,
                reconcile: String::new(),
            },
        ];
        app.workspace_infos
            .insert("orchestrator".into(), workspace_info("orchestrator", dirty));
        app.workspace_infos
            .insert("cli".into(), workspace_info("cli", clean));

        let (summary, _) = group_git_summary(0, &app, true).expect("group git summary");
        assert_eq!(summary, "mixed");

        let rendered = render_app_to_string(&mut app, 120, 18);
        assert!(rendered.contains("▾ ax git mixed"));
    }

    #[test]
    fn git_summary_suffix_truncates_inside_name_column() {
        let mut spans = Vec::new();
        push_padded_split_span(
            &mut spans,
            "▾ very-long-project-name",
            " git mixed",
            20,
            Style::default(),
            theme::severity(Severity::Warning),
        );

        let rendered = span_text(&spans);
        assert_eq!(rendered.chars().count(), 20);
        assert!(rendered.ends_with(" git mixed"));
    }

    #[test]
    fn group_git_summary_does_not_duplicate_nested_group_statuses() {
        let git = WorkspaceGitStatus {
            state: "dirty".into(),
            modified: 2,
            untracked: 1,
            ..WorkspaceGitStatus::default()
        };
        let mut app = App::new();
        app.agent_entries = vec![
            AgentEntry {
                label: "▾ ax".into(),
                workspace: String::new(),
                session_index: None,
                level: 0,
                group: true,
                reconcile: String::new(),
            },
            AgentEntry {
                label: "▾ child".into(),
                workspace: String::new(),
                session_index: None,
                level: 1,
                group: true,
                reconcile: String::new(),
            },
            AgentEntry {
                label: "worker".into(),
                workspace: "child.worker".into(),
                session_index: Some(0),
                level: 2,
                group: false,
                reconcile: String::new(),
            },
        ];
        app.workspace_infos
            .insert("child.worker".into(), workspace_info("child.worker", git));

        assert!(group_git_summary(0, &app, true).is_none());
        let (summary, _) = group_git_summary(1, &app, true).expect("child group git summary");
        assert_eq!(summary, "changed:2 ?1");
    }

    #[test]
    fn task_row_uses_column_spans_for_scanability() {
        let mut task = mock_task();
        task.status = TaskStatus::Failed;
        task.assignee = "ax.cli".into();

        let line = format_task_row(&task, 80, false, true);
        assert!(line.spans.len() >= 7);
        assert_eq!(line.spans[0].content.trim(), "abc");
        assert_eq!(line.spans[2].content.trim(), "failed");
        assert_eq!(line.spans[4].content.trim(), "ax.cli");
        assert!(line.spans[2].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn message_line_splits_time_sender_task_and_body_spans() {
        let entry = ax_daemon::HistoryEntry {
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-04-21T04:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            from: "ax.orchestrator".into(),
            to: "ax.cli".into(),
            content: "blocked on review".into(),
            task_id: "abcdef123456".into(),
        };

        let line = message_list_line(&entry, 96, false, true);
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.contains("04:00:00"));
        assert!(rendered.contains("ax.orchestrator"));
        assert!(rendered.contains("ax.orchestrator → ax.cli"));
        assert!(rendered.contains("[abcdef12]"));
        assert!(rendered.contains("blocked on review"));
    }

    #[test]
    fn message_line_labels_mcp_tool_activity_without_message_arrow() {
        let entry = ax_daemon::HistoryEntry {
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-04-21T04:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            from: "alpha".into(),
            to: "ax.daemon".into(),
            content: "mcp tool start_task error duration_ms=34 error_kind=TOOL_ERROR".into(),
            task_id: "abcdef123456".into(),
        };

        let line = message_list_line(&entry, 120, false, true);
        let rendered = span_text(&line.spans);
        assert!(rendered.contains("04:00:00"));
        assert!(rendered.contains("alpha used start_task [abcdef12]: error duration_ms=34"));
        assert!(rendered.contains("error_kind=TOOL_ERROR"));
        assert!(!rendered.contains("alpha → ax.daemon"));
        assert!(!rendered.contains("mcp tool"));
    }

    #[test]
    fn git_status_inline_includes_changed_and_untracked_counts() {
        let git = WorkspaceGitStatus {
            state: "dirty".into(),
            modified: 2,
            added: 1,
            deleted: 0,
            untracked: 3,
            files_changed: 4,
            insertions: 10,
            deletions: 2,
            message: String::new(),
        };

        assert_eq!(format_git_status_inline(&git, true), "changed:3 ?3");
        assert_eq!(format_git_status_inline(&git, false), "~3 ?3");
        assert_eq!(
            format_git_status_detail(&git),
            "dirty · modified 2 · added 1 · deleted 0 · untracked 3 · diff 4 files +10 -2"
        );
    }

    #[test]
    fn git_status_inline_handles_unavailable_states() {
        let git = WorkspaceGitStatus {
            state: "inaccessible".into(),
            message: "permission denied".into(),
            ..WorkspaceGitStatus::default()
        };

        assert_eq!(format_git_status_inline(&git, true), "no access");
        assert_eq!(
            format_git_status_detail(&git),
            "inaccessible: permission denied"
        );
    }

    #[test]
    fn git_status_inline_marks_clean_repos() {
        let git = WorkspaceGitStatus {
            state: "clean".into(),
            ..WorkspaceGitStatus::default()
        };

        assert_eq!(format_git_status_inline(&git, true), "clean");
        assert_eq!(format_git_status_inline(&git, false), "clean");
    }

    fn workspace_info(name: &str, git_status: WorkspaceGitStatus) -> WorkspaceInfo {
        WorkspaceInfo {
            name: name.into(),
            dir: String::new(),
            description: String::new(),
            status: AgentStatus::Online,
            status_text: String::new(),
            git_status: Some(git_status),
            connected_at: None,
            last_activity_at: None,
            active_task_count: 0,
            current_task_id: None,
        }
    }

    fn token_trend(name: &str, input: i64) -> WorkspaceTrend {
        WorkspaceTrend {
            workspace: name.into(),
            available: true,
            total: Tokens {
                input,
                output: 1,
                cache_read: 0,
                cache_creation: 0,
            },
            latest_model: "gpt-test".into(),
            ..WorkspaceTrend::default()
        }
    }

    fn token_trend_with_series(name: &str, series: &[i64]) -> WorkspaceTrend {
        let mut trend = token_trend(name, series.iter().sum::<i64>().max(1));
        trend.buckets = series
            .iter()
            .map(|total| UsageBucket {
                totals: Tokens {
                    input: *total,
                    output: 0,
                    cache_read: 0,
                    cache_creation: 0,
                },
                ..UsageBucket::default()
            })
            .collect();
        trend
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

    fn render_app_to_string(app: &mut App, width: u16, height: u16) -> String {
        let buffer = render_app_to_buffer(app, width, height);
        buffer_to_string(&buffer, width, height)
    }

    fn render_app_to_buffer(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, app)).expect("draw");
        terminal.backend().buffer().clone()
    }

    fn buffer_to_string(buffer: &ratatui::buffer::Buffer, width: u16, height: u16) -> String {
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn first_cell_for(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        height: u16,
        needle: &str,
    ) -> Option<(u16, u16)> {
        for y in 0..height {
            let mut line = String::new();
            for x in 0..width {
                line.push_str(buffer[(x, y)].symbol());
            }
            if let Some(byte_x) = line.find(needle) {
                let x = line[..byte_x].chars().count() as u16;
                return Some((x, y));
            }
        }
        None
    }

    fn first_cell_for_on_row(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        y: u16,
        needle: &str,
    ) -> Option<(u16, u16)> {
        let mut line = String::new();
        for x in 0..width {
            line.push_str(buffer[(x, y)].symbol());
        }
        let byte_x = line.find(needle)?;
        let x = line[..byte_x].chars().count() as u16;
        Some((x, y))
    }

    fn first_non_space_after(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        y: u16,
        start: u16,
    ) -> Option<(u16, u16)> {
        for x in start..width {
            if buffer[(x, y)].symbol() != " " {
                return Some((x, y));
            }
        }
        None
    }

    fn span_text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }
}
