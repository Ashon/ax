//! ratatui draw routine. This first slice renders a header + a
//! simple workspace/agent list so the crate boots end-to-end; the
//! full Bubbletea grid with tmux pane captures and the token
//! sidebar land in follow-up slices.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use ax_proto::types::AgentStatus;

use crate::state::App;

pub(crate) fn draw(f: &mut Frame, app: &App) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(size);

    draw_header(f, chunks[0], app);
    draw_workspace_list(f, chunks[1], app);
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

fn draw_workspace_list(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = if app.sessions.is_empty() && app.workspace_infos.is_empty() {
        vec![ListItem::new(
            "No active agents or tmux sessions yet. Run `ax up` from a project with .ax/config.yaml.",
        )]
    } else {
        let mut names: Vec<String> = app
            .workspace_infos
            .keys()
            .cloned()
            .chain(app.sessions.iter().map(|s| s.workspace.clone()))
            .collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .enumerate()
            .map(|(idx, name)| {
                let running = app.sessions.iter().any(|s| s.workspace == name);
                let status = app
                    .workspace_infos
                    .get(&name)
                    .map_or("offline", |ws| agent_status_str(&ws.status));
                let marker = if running { "●" } else { "○" };
                let label = format!("{marker} {name:<22} {status:<12}");
                let style = if idx == app.selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(label).style(style)
            })
            .collect()
    };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Workspaces"));
    f.render_widget(list, area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let text = match &app.notice {
        Some(msg) => msg.clone(),
        None => "j/k: move · q: quit · (grid view; stream view lands in next slice)".to_owned(),
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
