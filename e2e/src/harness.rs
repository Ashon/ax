//! Live orchestration harness. The helpers in this module own every
//! side-effect the live e2e takes (building ax, copying a fixture,
//! seeding codex auth, booting the daemon, driving a real tmux
//! session), so individual scenarios stay declarative: "use this
//! fixture, send this prompt, pass when validate.sh exits 0".
//!
//! Everything is gated by `AX_E2E_LIVE=1` at the test entry point.
//! The harness itself is pure — `cargo test` still compiles it, but
//! nothing in here spawns tmux/codex until a test opts in.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Env vars stripped from the inherited environment before the
/// sandbox runs anything. `TMUX*` leaks the host tmux server into
/// the sandbox — disastrous for session name collisions; `AX_*`
/// leaks user state we want isolated.
const STRIPPED_ENV: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "AX_SOCKET",
    "AX_CONFIG",
    "AX_TELEMETRY_DIR",
    "AX_TELEMETRY_DISABLED",
];

/// Required external tools. If any is missing the scenario skips
/// (the harness returns `Err(HarnessError::MissingTool)` and the
/// caller translates that into a `#[test]` skip).
const REQUIRED_TOOLS: &[&str] = &["tmux", "codex", "cargo"];

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("required external tool {0} is not on PATH")]
    MissingTool(String),
    #[error("host codex auth not found at {path:?} — cannot seed sandbox")]
    MissingCodexAuth { path: PathBuf },
    #[error("host HOME unset; required for codex auth seeding")]
    HomeUnset,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("command {cmd:?} failed (exit {status:?}): {combined}")]
    CommandFailed {
        cmd: String,
        status: Option<i32>,
        combined: String,
    },
    #[error("timed out after {after:?} waiting for: {what}")]
    Timeout { what: String, after: Duration },
}

/// Resolve the repo root by walking up from this source file. Used
/// so harness callers don't need to pass paths around.
#[must_use]
pub fn repo_root() -> PathBuf {
    // `e2e/src/harness.rs` → repo root is 3 levels up.
    let this = Path::new(env!("CARGO_MANIFEST_DIR")); // e2e/
    this.parent()
        .expect("e2e has a parent")
        .to_path_buf()
}

/// Isolated sandbox: one tempdir with every path the harness needs.
///
/// Drop order matters — the tempdir is cleaned last. Tests pull the
/// relevant paths as `&Path` references and hand them to
/// sub-helpers.
pub struct Sandbox {
    _tmp: TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
    socket: PathBuf,
    ax_bin: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
}

impl Sandbox {
    /// Create a new sandbox. Preflights external tool availability
    /// and host codex auth; if anything is missing the error is
    /// returned so the caller can translate into a skip.
    pub fn new() -> Result<Self, HarnessError> {
        for tool in REQUIRED_TOOLS {
            ensure_tool(tool)?;
        }
        let tmp = tempfile::Builder::new()
            .prefix("ax-e2e-")
            .tempdir()
            .map_err(HarnessError::Io)?;
        let root = tmp.path().to_path_buf();
        let home = root.join("h");
        let state = root.join("s");
        let tmux_tmp = root.join("t");
        let project = root.join("p");
        let socket = root.join("d.sock");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&state)?;
        std::fs::create_dir_all(&tmux_tmp)?;

        let env = build_sandbox_env(&home, &state, &tmux_tmp);

        let sb = Self {
            _tmp: tmp,
            root,
            home,
            project,
            socket,
            ax_bin: None,
            env,
        };
        sb.seed_codex_auth()?;
        Ok(sb)
    }

    pub fn env(&self) -> &[(OsString, OsString)] {
        &self.env
    }
    pub fn project(&self) -> &Path {
        &self.project
    }
    pub fn home(&self) -> &Path {
        &self.home
    }
    pub fn socket(&self) -> &Path {
        &self.socket
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config_path(&self) -> PathBuf {
        self.project.join(".ax").join("config.yaml")
    }

    /// Symlink `~/.codex/auth.json` into the sandbox so codex can
    /// authenticate with the host account but stays otherwise
    /// isolated from host config (no `config.toml` carried over).
    fn seed_codex_auth(&self) -> Result<(), HarnessError> {
        let host_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(HarnessError::HomeUnset)?;
        let auth_src = host_home.join(".codex").join("auth.json");
        if !auth_src.exists() {
            return Err(HarnessError::MissingCodexAuth { path: auth_src });
        }
        let target_dir = self.home.join(".codex");
        std::fs::create_dir_all(&target_dir)?;
        let target = target_dir.join("auth.json");
        if target.exists() {
            std::fs::remove_file(&target)?;
        }
        std::os::unix::fs::symlink(&auth_src, &target)?;
        Ok(())
    }

    /// Build the current checkout's `ax` binary in release mode
    /// into a stable cache under the sandbox. We build against the
    /// real workspace target dir so incremental runs reuse artifacts
    /// across scenarios.
    pub fn build_ax(&mut self) -> Result<PathBuf, HarnessError> {
        if let Some(p) = &self.ax_bin {
            return Ok(p.clone());
        }
        let repo = repo_root();
        run_logged(
            "cargo",
            ["build", "--release", "--bin", "ax"].iter().copied(),
            Some(&repo),
            &[],
        )?;
        let built = repo.join("target").join("release").join("ax");
        if !built.exists() {
            return Err(HarnessError::CommandFailed {
                cmd: "cargo build --release --bin ax".to_owned(),
                status: None,
                combined: format!(
                    "expected binary at {} did not appear",
                    built.display()
                ),
            });
        }
        // Copy so the sandbox owns the path we hand to child
        // processes; a later `cargo build` elsewhere can't surprise
        // us mid-scenario.
        let local = self.root.join("ax");
        std::fs::copy(&built, &local)?;
        self.ax_bin = Some(local.clone());
        Ok(local)
    }

    /// Copy a scenario fixture directory into `<sandbox>/p`.
    pub fn copy_scenario(&self, fixture_dir: &Path) -> Result<(), HarnessError> {
        copy_tree(fixture_dir, &self.project)?;
        Ok(())
    }
}

/// Running daemon handle — `Drop` kills the subprocess and removes
/// the socket so subsequent scenarios in the same test binary don't
/// collide.
pub struct DaemonProc {
    child: Child,
    socket: PathBuf,
}

impl DaemonProc {
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for DaemonProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

pub fn start_daemon(sandbox: &Sandbox, ax: &Path) -> Result<DaemonProc, HarnessError> {
    let sock = sandbox.socket().to_path_buf();
    let mut cmd = Command::new(ax);
    cmd.args(["--socket"])
        .arg(&sock)
        .args(["daemon", "start"])
        .current_dir(sandbox.project().parent().unwrap_or(sandbox.root()))
        .env_clear()
        .envs(sandbox.env().iter().map(|(k, v)| (k, v)))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = cmd.spawn()?;
    if wait_until(Duration::from_secs(10), Duration::from_millis(100), || {
        sock.exists()
    })
    .is_err()
    {
        return Err(HarnessError::Timeout {
            what: "daemon socket appear".into(),
            after: Duration::from_secs(10),
        });
    }
    Ok(DaemonProc { child, socket: sock })
}

/// Drive `ax init` blocking inside the sandbox. The subprocess runs
/// codex/claude as a child, so this call takes as long as the
/// setup agent needs. Stdout/stderr are captured so a failure
/// surfaces whatever the setup agent printed.
pub fn run_ax_init(sandbox: &Sandbox, ax: &Path, extra_args: &[&str]) -> Result<(), HarnessError> {
    let mut args: Vec<&str> = vec!["init"];
    args.extend_from_slice(extra_args);
    run_logged(
        ax.to_string_lossy().as_ref(),
        args,
        Some(sandbox.project()),
        sandbox.env(),
    )
}

pub fn ax_up(sandbox: &Sandbox, ax: &Path) -> Result<(), HarnessError> {
    run_logged(
        ax.to_string_lossy().as_ref(),
        [
            "--config",
            sandbox.config_path().to_string_lossy().as_ref(),
            "--socket",
            sandbox.socket().to_string_lossy().as_ref(),
            "up",
        ]
        .iter()
        .copied(),
        Some(sandbox.project()),
        sandbox.env(),
    )
}

pub fn ax_down(sandbox: &Sandbox, ax: &Path) {
    let _ = run_logged(
        ax.to_string_lossy().as_ref(),
        [
            "--config",
            sandbox.config_path().to_string_lossy().as_ref(),
            "--socket",
            sandbox.socket().to_string_lossy().as_ref(),
            "down",
        ]
        .iter()
        .copied(),
        Some(sandbox.project()),
        sandbox.env(),
    );
}

/// Owned tmux session; Drop kills it.
pub struct OrchestratorSession<'a> {
    sandbox: &'a Sandbox,
    name: String,
}

impl OrchestratorSession<'_> {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn capture_pane(&self) -> String {
        run_capture(
            "tmux",
            ["capture-pane", "-t", &self.name, "-p"]
                .iter()
                .copied(),
            None,
            self.sandbox.env(),
        )
        .unwrap_or_default()
    }

    /// Returns true when the last non-empty pane line looks like a
    /// prompt (codex `❯` or `›`, or a shell prompt character). Used
    /// to detect "the agent finished and is waiting for input".
    pub fn looks_idle(&self) -> bool {
        pane_looks_idle(&self.capture_pane())
    }

    pub fn wait_idle(&self, timeout: Duration) -> Result<(), HarnessError> {
        if wait_until(timeout, Duration::from_secs(2), || self.looks_idle()).is_err() {
            return Err(HarnessError::Timeout {
                what: "orchestrator pane idle".into(),
                after: timeout,
            });
        }
        Ok(())
    }

    /// Paste the prompt literally then press Enter. Codex's TUI
    /// occasionally swallows the first Enter while still painting
    /// its own prompt bar; re-press every ~3s until the submitted
    /// prompt shows up in codex history.
    pub fn send_prompt(&self, prompt: &str) -> Result<(), HarnessError> {
        run_logged(
            "tmux",
            ["send-keys", "-t", &self.name, "-l", prompt].iter().copied(),
            None,
            self.sandbox.env(),
        )?;
        std::thread::sleep(Duration::from_millis(150));
        run_logged(
            "tmux",
            ["send-keys", "-t", &self.name, "Enter"].iter().copied(),
            None,
            self.sandbox.env(),
        )?;
        // Best-effort confirm — keep pressing Enter until the prompt
        // text appears in codex's session history, or 20s elapses.
        let history_dir = self
            .sandbox
            .home()
            .join(".ax")
            .join("codex");
        let started = Instant::now();
        let deadline = started + Duration::from_secs(20);
        let mut last_enter = Instant::now();
        while Instant::now() < deadline {
            if codex_history_contains(&history_dir, prompt) {
                return Ok(());
            }
            if last_enter.elapsed() >= Duration::from_secs(3) {
                let _ = run_logged(
                    "tmux",
                    ["send-keys", "-t", &self.name, "Enter"].iter().copied(),
                    None,
                    self.sandbox.env(),
                );
                last_enter = Instant::now();
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Ok(())
    }
}

impl Drop for OrchestratorSession<'_> {
    fn drop(&mut self) {
        let _ = run_logged(
            "tmux",
            ["kill-session", "-t", &self.name].iter().copied(),
            None,
            self.sandbox.env(),
        );
    }
}

pub fn start_root_orchestrator<'a>(
    sandbox: &'a Sandbox,
    ax: &Path,
) -> Result<OrchestratorSession<'a>, HarnessError> {
    let name = format!(
        "ax-e2e-root-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let orch_dir = sandbox.home().join(".ax").join("orchestrator");
    std::fs::create_dir_all(&orch_dir)?;
    run_logged(
        "tmux",
        [
            "new-session",
            "-d",
            "-s",
            &name,
            "-c",
            orch_dir.to_string_lossy().as_ref(),
            ax.to_string_lossy().as_ref(),
            "run-agent",
            "--runtime",
            "codex",
            "--workspace",
            "orchestrator",
            "--socket",
            sandbox.socket().to_string_lossy().as_ref(),
            "--config",
            sandbox.config_path().to_string_lossy().as_ref(),
        ]
        .iter()
        .copied(),
        None,
        sandbox.env(),
    )?;
    Ok(OrchestratorSession { sandbox, name })
}

/// Runs the scenario's validate.sh in the project root. Returns
/// `Ok(())` when the script exits 0, `Err` otherwise with the
/// script's combined output attached.
pub fn run_validate_script(sandbox: &Sandbox, script_name: &str) -> Result<(), HarnessError> {
    let script = sandbox.project().join(script_name);
    if !script.exists() {
        return Err(HarnessError::CommandFailed {
            cmd: script_name.to_owned(),
            status: None,
            combined: format!(
                "validate script not found at {}",
                script.display()
            ),
        });
    }
    run_logged(
        "sh",
        [script.to_string_lossy().as_ref()].iter().copied(),
        Some(sandbox.project()),
        sandbox.env(),
    )
}

/// Pick the L1-style "settled success" predicate: validate.sh
/// passes AND the orchestrator pane has been idle continuously for
/// `settle_window`. The 15s settle window keeps multi-agent teams
/// from reporting success while a worker still has a response
/// in-flight.
pub fn wait_for_settled_success<F>(
    timeout: Duration,
    poll_interval: Duration,
    settle_window: Duration,
    mut predicate: F,
) -> Result<(), HarnessError>
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    let mut settled_at: Option<Instant> = None;
    while Instant::now() < deadline {
        if predicate() {
            let anchor = *settled_at.get_or_insert_with(Instant::now);
            if anchor.elapsed() >= settle_window {
                return Ok(());
            }
        } else {
            settled_at = None;
        }
        std::thread::sleep(poll_interval);
    }
    Err(HarnessError::Timeout {
        what: "settled-success predicate".into(),
        after: timeout,
    })
}

// ---------- private helpers ----------

fn build_sandbox_env(home: &Path, state: &Path, tmux_tmp: &Path) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            !STRIPPED_ENV.iter().any(|s| *s == k)
        })
        .collect();
    let mut upsert = |k: &str, v: OsString| {
        if let Some(slot) = env.iter_mut().find(|(ek, _)| ek == k) {
            slot.1 = v;
        } else {
            env.push((k.into(), v));
        }
    };
    upsert("HOME", home.as_os_str().to_owned());
    upsert("XDG_STATE_HOME", state.as_os_str().to_owned());
    upsert("TMUX_TMPDIR", tmux_tmp.as_os_str().to_owned());
    upsert("NO_COLOR", OsString::from("1"));
    env
}

fn ensure_tool(name: &str) -> Result<(), HarnessError> {
    let out = Command::new("which")
        .arg(name)
        .output()
        .map_err(HarnessError::Io)?;
    if out.status.success() && !out.stdout.is_empty() {
        Ok(())
    } else {
        Err(HarnessError::MissingTool(name.into()))
    }
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
        // Preserve the exec bit for validate.sh etc.
        if let Ok(meta) = std::fs::metadata(src) {
            let _ = std::fs::set_permissions(dst, meta.permissions());
        }
    }
    Ok(())
}

fn run_logged<'a, I>(
    cmd: &str,
    args: I,
    dir: Option<&Path>,
    env: &[(OsString, OsString)],
) -> Result<(), HarnessError>
where
    I: IntoIterator<Item = &'a str>,
{
    let args: Vec<&str> = args.into_iter().collect();
    let mut c = Command::new(cmd);
    c.args(&args);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    if !env.is_empty() {
        c.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = c.output()?;
    if out.status.success() {
        return Ok(());
    }
    let mut combined = Vec::with_capacity(out.stdout.len() + out.stderr.len());
    combined.write_all(&out.stdout)?;
    combined.write_all(&out.stderr)?;
    Err(HarnessError::CommandFailed {
        cmd: format!("{cmd} {}", args.join(" ")),
        status: out.status.code(),
        combined: String::from_utf8_lossy(&combined).trim().to_owned(),
    })
}

fn run_capture<'a, I>(
    cmd: &str,
    args: I,
    dir: Option<&Path>,
    env: &[(OsString, OsString)],
) -> Result<String, HarnessError>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut c = Command::new(cmd);
    c.args(args.into_iter().collect::<Vec<_>>());
    if let Some(d) = dir {
        c.current_dir(d);
    }
    if !env.is_empty() {
        c.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    let out = c.output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn wait_until<F>(timeout: Duration, poll: Duration, mut pred: F) -> Result<(), ()>
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return Ok(());
        }
        std::thread::sleep(poll);
    }
    Err(())
}

fn pane_looks_idle(content: &str) -> bool {
    let lines: Vec<&str> = content.trim_end().lines().collect();
    let mut checked = 0;
    for line in lines.iter().rev() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        checked += 1;
        if t.ends_with('❯')
            || t == "❯"
            || t.starts_with('›')
            || t == ">"
            || t == "$"
            || t == "#"
            || t == "claude>"
        {
            return true;
        }
        if checked >= 4 {
            return false;
        }
    }
    false
}

fn codex_history_contains(history_dir: &Path, needle: &str) -> bool {
    let Ok(read) = std::fs::read_dir(history_dir) else {
        return false;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.starts_with("orchestrator"))
        {
            continue;
        }
        let hist = path.join("history.jsonl");
        if let Ok(body) = std::fs::read_to_string(&hist) {
            if body.contains(needle) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_idle_detection_matches_prompt_suffixes() {
        assert!(pane_looks_idle("some output\n❯"));
        assert!(pane_looks_idle("banner\n  codex ready\n›"));
        assert!(pane_looks_idle("\n$"));
        assert!(!pane_looks_idle("  working...\n...processing"));
    }

    #[test]
    fn pane_idle_detection_ignores_trailing_blanks() {
        assert!(pane_looks_idle("...\n❯\n\n\n"));
    }
}
