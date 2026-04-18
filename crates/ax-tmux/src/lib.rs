//! Thin synchronous wrappers around the `tmux` CLI.
//!
//! Every entry point shells out to `tmux` via
//! [`std::process::Command`]; no tmux library binding. Async callers
//! wrap the blocking calls in `tokio::task::spawn_blocking`.
//!
//! Session-name convention: workspace names are ASCII with `.` → `_`
//! substitution and prefixed with `ax-`.

#![forbid(unsafe_code)]

mod commands;
mod keys;
mod sessions;

pub use commands::{
    capture_pane, create_ephemeral_session, create_session, create_session_with_args,
    create_session_with_command, destroy_session, interrupt_workspace, is_idle, list_sessions,
    parse_list_sessions_stdout, send_keys, send_raw_key, send_special_key_to_session,
    send_special_keys, session_exists, wake_workspace, CreateOptions, SessionInfo,
};
pub use keys::{resolve_key_token, ResolvedKey};
pub use sessions::{
    attach_session, decode_workspace_name, encode_workspace_name, is_inside_tmux, session_name,
    SESSION_PREFIX,
};

#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("tmux {op}: {message}")]
    Command { op: String, message: String },
    #[error("tmux session for workspace {workspace:?} not found")]
    SessionNotFound { workspace: String },
    #[error("parse tmux output: {0}")]
    Parse(String),
    #[error("tmux exec: {0}")]
    Io(#[from] std::io::Error),
}
