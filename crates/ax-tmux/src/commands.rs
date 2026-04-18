//! tmux command wrappers. Each function shells out to `tmux`, captures
//! combined output (so the error message is useful), and turns exit
//! failures into [`TmuxError::Command`].

use std::collections::BTreeMap;
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use crate::keys::{resolve_key_token, ResolvedKey};
use crate::sessions::session_name;
use crate::TmuxError;

/// Trailing characters that indicate a pane is sitting at an interactive
/// prompt. Matches the list used by `tmux.IsIdle` in Go.
const IDLE_PROMPT_SUFFIXES: [&str; 5] = ["❯", "> ", "$ ", "# ", "claude>"];

/// Session listing entry emitted by [`list_sessions`]. Mirrors
/// `tmux.SessionInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub name: String,
    pub workspace: String,
    pub attached: bool,
    pub windows: u32,
}

/// Options shared by the `create_session_*` calls.
#[derive(Debug, Clone, Default)]
pub struct CreateOptions {
    pub env: BTreeMap<String, String>,
}

/// `tmux new-session -d -s <name> -c <dir> [-e K=V …] [<shell>]`.
pub fn create_session(
    workspace: &str,
    dir: &str,
    shell: &str,
    opts: &CreateOptions,
) -> Result<(), TmuxError> {
    let name = session_name(workspace);
    let mut args: Vec<String> = vec![
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name,
        "-c".into(),
        dir.into(),
    ];
    args.extend(env_args(&opts.env));
    if !shell.is_empty() {
        args.push(shell.into());
    }
    run_combined("new-session", &args)?;
    Ok(())
}

/// `tmux new-session … sh -c <command>` with `remain-on-exit` on so the
/// pane stays visible when the command exits (matches
/// `CreateSessionWithCommand` in Go).
pub fn create_session_with_command(
    workspace: &str,
    dir: &str,
    command: &str,
    opts: &CreateOptions,
) -> Result<(), TmuxError> {
    let name = session_name(workspace);
    let mut args: Vec<String> = vec![
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name.clone(),
        "-c".into(),
        dir.into(),
    ];
    args.extend(command_with_env(&["sh", "-c", command], &opts.env));
    run_combined("new-session", &args)?;
    set_remain_on_exit(&name)
}

/// Ephemeral session that terminates as soon as the command exits
/// (no `remain-on-exit`).
pub fn create_ephemeral_session(
    workspace: &str,
    dir: &str,
    argv: &[&str],
) -> Result<(), TmuxError> {
    let name = session_name(workspace);
    let mut cmd_args: Vec<String> = vec![
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name,
        "-c".into(),
        dir.into(),
    ];
    cmd_args.extend(argv.iter().map(|s| (*s).to_owned()));
    run_combined("new-session", &cmd_args)?;
    Ok(())
}

/// Long-lived session running `argv` with `remain-on-exit`. Equivalent to
/// `CreateSessionWithArgs` in Go.
pub fn create_session_with_args(
    workspace: &str,
    dir: &str,
    argv: &[&str],
    opts: &CreateOptions,
) -> Result<(), TmuxError> {
    let name = session_name(workspace);
    let owned: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    let mut cmd_args: Vec<String> = vec![
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name.clone(),
        "-c".into(),
        dir.into(),
    ];
    let owned_refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    cmd_args.extend(command_with_env(&owned_refs, &opts.env));
    run_combined("new-session", &cmd_args)?;
    set_remain_on_exit(&name)
}

/// `tmux kill-session -t <name>`.
pub fn destroy_session(workspace: &str) -> Result<(), TmuxError> {
    let name = session_name(workspace);
    run_combined("kill-session", &["kill-session", "-t", &name])?;
    Ok(())
}

/// `tmux list-sessions`. Silent when the tmux server isn't running.
pub fn list_sessions() -> Result<Vec<SessionInfo>, TmuxError> {
    let output = Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name} #{session_attached} #{session_windows}",
        ])
        .output()?;
    parse_list_sessions_result(&output)
}

/// `tmux has-session -t <name>`. Returns true on exit 0.
#[must_use]
pub fn session_exists(workspace: &str) -> bool {
    let name = session_name(workspace);
    Command::new("tmux")
        .args(["has-session", "-t", &name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Send named/literal keys to a workspace session, one key per send-keys
/// invocation (named keys go without `-l`, literals go with `-l`).
pub fn send_keys(workspace: &str, keys: &[&str]) -> Result<(), TmuxError> {
    if !session_exists(workspace) {
        return Err(TmuxError::SessionNotFound {
            workspace: workspace.to_owned(),
        });
    }
    let name = session_name(workspace);
    for k in keys {
        if k.is_empty() {
            continue;
        }
        match resolve_key_token(k) {
            ResolvedKey::Special(mapped) => {
                run_combined("send-keys", &["send-keys", "-t", &name, mapped])?;
            }
            ResolvedKey::Literal(text) => {
                run_combined("send-keys", &["send-keys", "-t", &name, "-l", &text])?;
            }
        }
    }
    Ok(())
}

/// Lower-level `send-keys` that skips the session-exists precheck and
/// passes all tokens to tmux verbatim.
pub fn send_special_keys(workspace: &str, keys: &[&str]) -> Result<(), TmuxError> {
    let name = session_name(workspace);
    let mut args: Vec<String> = vec!["send-keys".into(), "-t".into(), name];
    args.extend(keys.iter().map(|s| (*s).to_owned()));
    run_combined("send-keys", &args)?;
    Ok(())
}

/// Version of [`send_special_keys`] keyed by the raw tmux session name
/// (not a workspace). Used by the watch TUI for unregistered sessions.
pub fn send_special_key_to_session(session: &str, keys: &[&str]) -> Result<(), TmuxError> {
    let mut args: Vec<String> = vec!["send-keys".into(), "-t".into(), session.to_owned()];
    args.extend(keys.iter().map(|s| (*s).to_owned()));
    run_combined("send-keys", &args)?;
    Ok(())
}

/// Send literal text without appending Enter (`send-keys -l`).
pub fn send_raw_key(session: &str, key: &str) -> Result<(), TmuxError> {
    run_combined("send-keys", &["send-keys", "-t", session, "-l", key])?;
    Ok(())
}

/// Interrupt the foreground agent (Escape).
pub fn interrupt_workspace(workspace: &str) -> Result<(), TmuxError> {
    send_keys(workspace, &["Escape"])
}

/// Inject a wake-up prompt: Escape + C-u clears the composer, then type
/// the prompt, sleep briefly, then Enter to submit.
pub fn wake_workspace(workspace: &str, prompt: &str) -> Result<(), TmuxError> {
    let name = session_name(workspace);
    run_combined(
        "wake workspace (clear)",
        &["send-keys", "-t", &name, "Escape", "C-u"],
    )?;
    run_combined("wake workspace (type)", &["send-keys", "-t", &name, prompt])?;
    thread::sleep(Duration::from_millis(150));
    run_combined(
        "wake workspace (submit)",
        &["send-keys", "-t", &name, "Enter"],
    )?;
    Ok(())
}

/// `tmux capture-pane -t <name> -p [-e]`.
pub fn capture_pane(workspace: &str, with_escape: bool) -> Result<String, TmuxError> {
    let name = session_name(workspace);
    let mut args: Vec<String> = vec!["capture-pane".into(), "-t".into(), name, "-p".into()];
    if with_escape {
        args.push("-e".into());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = Command::new("tmux").args(&arg_refs).output()?;
    if !output.status.success() {
        return Err(TmuxError::Command {
            op: "capture-pane".into(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Heuristic idle detection: scan the last non-empty line of the pane for
/// a shell/agent prompt glyph.
#[must_use]
pub fn is_idle(workspace: &str) -> bool {
    let Ok(pane) = capture_pane(workspace, false) else {
        return false;
    };
    pane_looks_idle(&pane)
}

/// Extracted for unit testing; the production `is_idle` shells out and
/// then delegates to this function.
#[must_use]
pub(crate) fn pane_looks_idle(pane: &str) -> bool {
    let trimmed = pane.trim_end_matches('\n');
    let last_line = trimmed
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if last_line.is_empty() {
        return false;
    }
    for p in IDLE_PROMPT_SUFFIXES {
        if last_line.ends_with(p) || last_line == p.trim() {
            return true;
        }
    }
    last_line == ">" || last_line == "❯"
}

// ---------- internals ----------

fn set_remain_on_exit(session_name: &str) -> Result<(), TmuxError> {
    let args = ["set-option", "-t", session_name, "remain-on-exit", "on"];
    match run_combined("set-option remain-on-exit", &args) {
        Ok(_) => Ok(()),
        Err(e) => {
            // Best-effort cleanup matches the Go helper.
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", session_name])
                .status();
            Err(e)
        }
    }
}

fn env_args(env: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::with_capacity(env.len() * 2);
    for (k, v) in env {
        out.push("-e".to_owned());
        out.push(format!("{k}={v}"));
    }
    out
}

fn command_with_env(argv: &[&str], env: &BTreeMap<String, String>) -> Vec<String> {
    if env.is_empty() {
        return argv.iter().map(|s| (*s).to_owned()).collect();
    }
    let mut out: Vec<String> = vec!["env".to_owned()];
    for (k, v) in env {
        out.push(format!("{k}={v}"));
    }
    out.extend(argv.iter().map(|s| (*s).to_owned()));
    out
}

/// Run tmux with `args` and `output()` semantics matching Go's
/// `CombinedOutput` — both stdout and stderr merged into the error body.
fn run_combined<S: AsRef<str>>(op: &str, args: &[S]) -> Result<Vec<u8>, TmuxError> {
    let output = Command::new("tmux")
        .args(args.iter().map(AsRef::as_ref))
        .output()?;
    if !output.status.success() {
        let mut combined = Vec::with_capacity(output.stdout.len() + output.stderr.len());
        combined.extend_from_slice(&output.stdout);
        combined.extend_from_slice(&output.stderr);
        let message = String::from_utf8_lossy(&combined).trim().to_owned();
        return Err(TmuxError::Command {
            op: op.to_owned(),
            message,
        });
    }
    Ok(output.stdout)
}

/// Parse the output of `tmux list-sessions` into [`SessionInfo`] entries.
/// Publicly exposed for unit testing; the production [`list_sessions`]
/// wraps this with an `Output` from a real subprocess.
pub(crate) fn parse_list_sessions_result(output: &Output) -> Result<Vec<SessionInfo>, TmuxError> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let combined = format!("{}{}", stdout, String::from_utf8_lossy(&output.stderr));
        // "no server running" is how tmux signals an empty-but-healthy
        // state; translate to an empty list like Go does.
        if combined.contains("no server running") {
            return Ok(Vec::new());
        }
        return Err(TmuxError::Command {
            op: "list-sessions".into(),
            message: combined.trim().to_owned(),
        });
    }
    parse_list_sessions_stdout(&stdout)
}

// `Result` return type mirrors the Go caller signature so future parse
// failures can be surfaced without an API break. Clippy flags it as
// unnecessary today because the current body always succeeds.
#[allow(clippy::unnecessary_wraps)]
pub fn parse_list_sessions_stdout(stdout: &str) -> Result<Vec<SessionInfo>, TmuxError> {
    use crate::sessions::decode_workspace_name;
    let mut sessions = Vec::new();
    for line in stdout.trim().lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            continue;
        }
        let name = parts[0].to_owned();
        let Some(encoded) = name.strip_prefix(crate::sessions::SESSION_PREFIX) else {
            continue;
        };
        let attached = parts[1] == "1";
        let windows = parts[2].parse::<u32>().unwrap_or(1);
        sessions.push(SessionInfo {
            name: name.clone(),
            workspace: decode_workspace_name(encoded),
            attached,
            windows,
        });
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_detects_shell_and_agent_prompts() {
        assert!(pane_looks_idle("running task…\n$ "));
        assert!(pane_looks_idle("> "));
        assert!(pane_looks_idle("logs...\n❯"));
        assert!(pane_looks_idle("claude>"));
    }

    #[test]
    fn idle_rejects_active_and_empty_panes() {
        assert!(!pane_looks_idle(""));
        assert!(!pane_looks_idle("Working on the task..."));
        assert!(!pane_looks_idle("thinking..."));
    }

    #[test]
    fn parse_list_sessions_stdout_filters_prefix_and_decodes_name() {
        let sample = "\
ax-ax_cli 0 2
ax-team_worker 1 1
other-session 0 1
malformed line
";
        let sessions = parse_list_sessions_stdout(sample).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].workspace, "ax.cli");
        assert!(!sessions[0].attached);
        assert_eq!(sessions[0].windows, 2);
        assert_eq!(sessions[1].workspace, "team.worker");
        assert!(sessions[1].attached);
    }

    #[test]
    fn parse_list_sessions_stdout_on_empty_returns_empty() {
        let sessions = parse_list_sessions_stdout("").unwrap();
        assert!(sessions.is_empty());
    }
}
