//! ratatui watch TUI for `ax top` / `ax watch`. Contains terminal
//! setup + teardown, the core `App` state, a sync daemon client for
//! list-workspaces, the sidebar, stream pane, quick actions, tmux
//! captures, and the tokens/tasks views.

#![forbid(unsafe_code)]

mod actions;
mod app;
mod captures;
mod daemon;
mod input;
mod render;
mod sidebar;
mod state;
mod stream;
mod tasks;
mod terminal;
mod tokens;

pub use app::{run, RunError, RunOptions};
