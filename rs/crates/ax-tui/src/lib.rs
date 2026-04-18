//! ratatui port of the Go watch TUI (`cmd/watch_*.go`). This first
//! slice establishes the crate scaffold — terminal setup + teardown,
//! the core `App` state, a sync daemon client for list-workspaces,
//! and a placeholder grid view so the binary runs end-to-end. The
//! full feature set (sidebar, stream pane, quick actions, tmux
//! captures, trends) lands in follow-up slices.

#![forbid(unsafe_code)]

mod actions;
mod app;
mod daemon;
mod input;
mod render;
mod sidebar;
mod state;
mod stream;
mod tasks;
mod terminal;

pub use app::{run, RunError, RunOptions};
