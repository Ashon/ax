//! Main event loop — owns the state and drives render/input/refresh.
//! Mirrors Bubbletea's `NewProgram(model).Run()` but stays sync +
//! single-threaded since ratatui doesn't push messages through an
//! async runtime.
//!
//! Refresh cadence matches the Go TUI's `watchDataRefreshInterval`
//! (250ms): tmux sessions re-listed, daemon re-queried for workspace
//! info, view redrawn.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};

use crate::daemon::Client;
use crate::state::App;
use crate::terminal::TerminalGuard;

const REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub socket_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("terminal setup: {0}")]
    Terminal(std::io::Error),
    #[error("render: {0}")]
    Render(std::io::Error),
    #[error("input: {0}")]
    Input(std::io::Error),
    #[error("tmux: {0}")]
    Tmux(ax_tmux::TmuxError),
}

pub fn run(opts: &RunOptions) -> Result<(), RunError> {
    let mut guard = TerminalGuard::install().map_err(RunError::Terminal)?;
    let mut app = App::new();
    refresh(&mut app, opts);

    loop {
        guard
            .terminal
            .draw(|f| crate::render::draw(f, &app))
            .map_err(RunError::Render)?;

        if event::poll(POLL_INTERVAL).map_err(RunError::Input)? {
            if let Event::Key(key) = event::read().map_err(RunError::Input)? {
                crate::input::handle_key(&mut app, key);
            }
        }
        if app.quit {
            break;
        }

        let due = app
            .last_refresh
            .is_none_or(|t| t.elapsed() >= REFRESH_INTERVAL);
        if due {
            refresh(&mut app, opts);
        }
    }
    Ok(())
}

fn refresh(app: &mut App, opts: &RunOptions) {
    match ax_tmux::list_sessions() {
        Ok(sessions) => app.sessions = sessions,
        Err(e) => app.set_notice(format!("tmux list-sessions: {e}")),
    }

    if let Ok(mut client) = Client::connect(&opts.socket_path) {
        app.daemon_running = true;
        match client.list_workspaces() {
            Ok(workspaces) => {
                app.workspace_infos = workspaces
                    .into_iter()
                    .map(|ws| (ws.name.clone(), ws))
                    .collect();
            }
            Err(e) => app.set_notice(format!("daemon: {e}")),
        }
    } else {
        app.daemon_running = false;
        app.workspace_infos.clear();
    }

    // Re-read the config tree each tick so users editing ax.yaml see
    // changes take effect without restarting the TUI. Failures are
    // silent — a missing config just falls back to the name-split
    // fallback tree.
    refresh_tree(app);
    app.rebuild_sidebar();
    refresh_messages(app, opts);
    refresh_tasks(app, opts);
    app.last_refresh = Some(Instant::now());
}

const MESSAGE_HISTORY_BUFFER: usize = 500;

fn refresh_messages(app: &mut App, opts: &RunOptions) {
    let path = crate::stream::history_file_path(&opts.socket_path);
    app.messages = crate::stream::read_history(&path, MESSAGE_HISTORY_BUFFER);
}

fn refresh_tasks(app: &mut App, opts: &RunOptions) {
    let path = crate::tasks::tasks_file_path(&opts.socket_path);
    app.tasks = crate::tasks::read_tasks(&path);
}

fn refresh_tree(app: &mut App) {
    let Ok(cwd) = std::env::current_dir() else {
        app.tree = None;
        app.desired.clear();
        app.reconfigure_enabled = false;
        return;
    };
    let Some(cfg_path) = ax_config::find_config_file(cwd) else {
        app.tree = None;
        app.desired.clear();
        app.reconfigure_enabled = false;
        return;
    };
    let tree = ax_config::Config::load_tree(&cfg_path).ok();
    let reconfigure = ax_config::Config::load(&cfg_path)
        .map(|cfg| cfg.experimental_mcp_team_reconfigure)
        .unwrap_or(false);
    app.reconfigure_enabled = reconfigure;
    if let Some(ref tree) = tree {
        app.desired = build_desired_set(tree, reconfigure);
    } else {
        app.desired.clear();
    }
    app.tree = tree;
}

fn build_desired_set(
    tree: &ax_config::ProjectNode,
    reconfigure_enabled: bool,
) -> std::collections::BTreeMap<String, bool> {
    let mut out = std::collections::BTreeMap::new();
    if !reconfigure_enabled {
        return out;
    }
    walk_desired(tree, &mut out);
    out
}

fn walk_desired(node: &ax_config::ProjectNode, out: &mut std::collections::BTreeMap<String, bool>) {
    if !(node.prefix.is_empty() && node.disable_root_orchestrator) {
        let name = if node.prefix.is_empty() {
            "orchestrator".to_owned()
        } else {
            format!("{}.orchestrator", node.prefix)
        };
        out.insert(name, true);
    }
    for ws in &node.workspaces {
        out.insert(ws.merged_name.clone(), true);
    }
    for child in &node.children {
        walk_desired(child, out);
    }
}
