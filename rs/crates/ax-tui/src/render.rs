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

use crate::sidebar::SidebarEntry;
use crate::state::App;

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

    draw_footer(f, chunks[2], app);
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
    let (title, body): (&str, String) = match current_entry(app) {
        Some(entry) => {
            let ws = &entry.workspace;
            let info = app
                .workspace_infos
                .get(ws)
                .map_or_else(|| "(offline)".to_owned(), workspace_info_summary);
            (
                "selection",
                format!(
                    "workspace: {ws}\nsession: {}\nagent:    {}",
                    entry
                        .session_index
                        .and_then(|idx| app.sessions.get(idx))
                        .map_or_else(|| "none".to_owned(), |s| s.name.clone()),
                    info,
                ),
            )
        }
        None => (
            "body",
            "stream view lands in the next slice — pick a workspace on the left".to_owned(),
        ),
    };
    let para = Paragraph::new(body).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} ")),
    );
    f.render_widget(para, area);
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
    let text = match &app.notice {
        Some(msg) => msg.clone(),
        None => "j/k: move · q: quit · sidebar shows project tree + live sessions".to_owned(),
    };
    let footer = Paragraph::new(text).style(Style::default().add_modifier(Modifier::DIM));
    f.render_widget(footer, area);
}

fn agent_status_str(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::Online => "online",
        AgentStatus::Offline => "offline",
        AgentStatus::Disconnected => "disconnected",
    }
}
