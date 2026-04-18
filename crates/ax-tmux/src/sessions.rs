//! Session-name encoding and attach helpers.
//!
//! These are the only tmux calls that need to reuse stdin/stdout/stderr of
//! the current process so the interactive UI works. All other tmux calls
//! capture output and return it.

use std::process::{Command, Stdio};

use crate::TmuxError;

pub const SESSION_PREFIX: &str = "ax-";

/// Returns the tmux session name assigned to `workspace`
/// (`ax-<encoded>`).
#[must_use]
pub fn session_name(workspace: &str) -> String {
    format!("{SESSION_PREFIX}{}", encode_workspace_name(workspace))
}

/// Replace `.` with `_` so tmux session names stay single-token.
#[must_use]
pub fn encode_workspace_name(workspace: &str) -> String {
    workspace.replace('.', "_")
}

#[must_use]
pub fn decode_workspace_name(encoded: &str) -> String {
    encoded.replace('_', ".")
}

/// Report whether the current process is running inside a tmux client.
#[must_use]
pub fn is_inside_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Attach to (or, if already inside tmux, switch to) the workspace session.
/// The tmux process inherits stdin/stdout/stderr so the user can interact
/// with the attached session.
pub fn attach_session(workspace: &str) -> Result<(), TmuxError> {
    let name = session_name(workspace);
    let (subcommand, op) = if is_inside_tmux() {
        ("switch-client", "switch-client")
    } else {
        ("attach-session", "attach-session")
    };
    let status = Command::new("tmux")
        .arg(subcommand)
        .args(["-t", &name])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(TmuxError::Command {
            op: op.to_owned(),
            message: format!("exit status {status}"),
        });
    }
    Ok(())
}
