#![forbid(unsafe_code)]

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};

use ax_agent::{run_with_options, LaunchOptions};
use ax_config::find_config_file;
use ax_daemon::{expand_socket_path, Daemon, DEFAULT_SOCKET_PATH};
use ax_workspace::{
    dispatch_runnable_work, restart_named_target, start_named_target, stop_named_target, RealTmux,
};

const USAGE: &str = "\
ax-rs - thin Rust entrypoint for migrated workspace control

Usage:
  ax-rs daemon start [--socket PATH]
  ax-rs daemon stop [--socket PATH]
  ax-rs daemon status [--socket PATH]
  ax-rs start <target> [--config PATH] [--socket PATH] [--ax-bin PATH]
  ax-rs stop <target> [--config PATH] [--socket PATH] [--ax-bin PATH]
  ax-rs restart <target> [--config PATH] [--socket PATH] [--ax-bin PATH]
  ax-rs dispatch <target> --sender NAME [--fresh] [--config PATH] [--socket PATH] [--ax-bin PATH]
  ax-rs run-agent --workspace NAME [--runtime RUNTIME] [--socket PATH] [--config PATH] [--fresh] [-- ...]
  ax-rs mcp-server ...

Notes:
  --config defaults to the discovered ax config (.ax/config.yaml or ax.yaml)
  --socket defaults to ~/.local/state/ax/daemon.sock
  --ax-bin defaults to the current ax-rs executable
  ax-rs run-agent is handled natively; mcp-server is still delegated to Go ax
  Set AX_GO_BINARY=/path/to/ax to override the delegated Go binary for mcp-server (default: ax)
";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommonOptions {
    socket_path: PathBuf,
    config_path: PathBuf,
    ax_bin: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleAction {
    Start,
    Stop,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonAction {
    Start,
    Stop,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedCommand {
    Help,
    Daemon {
        action: DaemonAction,
        socket_path: PathBuf,
    },
    Lifecycle {
        action: LifecycleAction,
        target: String,
        options: CommonOptions,
    },
    RunAgent {
        runtime: String,
        workspace: String,
        socket_path: PathBuf,
        config_path: Option<PathBuf>,
        fresh: bool,
        extra_args: Vec<String>,
    },
    Dispatch {
        target: String,
        sender: String,
        fresh: bool,
        options: CommonOptions,
    },
    Delegate {
        argv: Vec<OsString>,
    },
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    Daemon(DaemonCliError),
    Lifecycle(ax_workspace::LifecycleError),
    Dispatch(ax_workspace::DispatchError),
    RunAgent(ax_agent::LaunchError),
    DelegateLaunch {
        binary: String,
        source: std::io::Error,
    },
    DelegateLoop {
        binary: String,
    },
}

#[derive(Debug)]
enum DaemonCliError {
    MissingStateDir {
        socket_path: PathBuf,
    },
    BuildRuntime {
        source: io::Error,
    },
    LoadState {
        state_dir: PathBuf,
        source: ax_daemon::DaemonError,
    },
    Bind(ax_daemon::DaemonError),
    SignalSetup {
        source: io::Error,
    },
    SignalWait {
        source: io::Error,
    },
    WritePid {
        path: PathBuf,
        source: io::Error,
    },
    ReadPid {
        path: PathBuf,
        source: io::Error,
    },
    MissingPidFile,
    InvalidPidFile,
    SignalCommand {
        signal: &'static str,
        source: io::Error,
    },
    SignalFailed {
        signal: &'static str,
        stderr: String,
    },
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            Self::Daemon(source) => write!(f, "{source}"),
            Self::Lifecycle(source) => write!(f, "{source}"),
            Self::Dispatch(source) => write!(f, "{source}"),
            Self::RunAgent(source) => write!(f, "{source}"),
            Self::DelegateLaunch { binary, source } => {
                write!(f, "launch delegated ax binary {binary:?}: {source}")
            }
            Self::DelegateLoop { binary } => {
                write!(f, "delegated ax binary {binary:?} resolves to ax-rs itself")
            }
        }
    }
}

impl fmt::Display for DaemonCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingStateDir { socket_path } => {
                write!(f, "resolve daemon state dir from socket {:?}", socket_path)
            }
            Self::BuildRuntime { source } => write!(f, "build tokio runtime: {source}"),
            Self::LoadState { state_dir, source } => {
                write!(f, "load daemon state from {:?}: {source}", state_dir)
            }
            Self::Bind(source) => write!(f, "{source}"),
            Self::SignalSetup { source } => write!(f, "install shutdown signal handler: {source}"),
            Self::SignalWait { source } => write!(f, "wait for shutdown signal: {source}"),
            Self::WritePid { path, source } => write!(f, "write pid file {:?}: {source}", path),
            Self::ReadPid { path, source } => write!(f, "read pid file {:?}: {source}", path),
            Self::MissingPidFile => f.write_str("daemon not running (no pid file)"),
            Self::InvalidPidFile => f.write_str("invalid pid file"),
            Self::SignalCommand { signal, source } => write!(f, "signal {signal}: {source}"),
            Self::SignalFailed { signal, stderr } => {
                if stderr.is_empty() {
                    write!(f, "signal {signal} failed")
                } else {
                    write!(f, "signal {signal} failed: {stderr}")
                }
            }
        }
    }
}

impl From<DaemonCliError> for CliError {
    fn from(source: DaemonCliError) -> Self {
        Self::Daemon(source)
    }
}

impl From<ax_workspace::LifecycleError> for CliError {
    fn from(source: ax_workspace::LifecycleError) -> Self {
        Self::Lifecycle(source)
    }
}

impl From<ax_workspace::DispatchError> for CliError {
    fn from(source: ax_workspace::DispatchError) -> Self {
        Self::Dispatch(source)
    }
}

impl From<ax_agent::LaunchError> for CliError {
    fn from(source: ax_agent::LaunchError) -> Self {
        Self::RunAgent(source)
    }
}

fn main() -> ExitCode {
    let cwd = match env::current_dir() {
        Ok(path) => path,
        Err(source) => {
            eprintln!("resolve current dir: {source}");
            return ExitCode::from(1);
        }
    };
    let current_exe = match env::current_exe() {
        Ok(path) => path,
        Err(source) => {
            eprintln!("resolve current executable: {source}");
            return ExitCode::from(1);
        }
    };

    match run(env::args_os(), &cwd, &current_exe) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}

fn run<I>(args: I, cwd: &Path, current_exe: &Path) -> Result<ExitCode, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    match parse_args(args, cwd, current_exe)? {
        ParsedCommand::Help => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        ParsedCommand::Daemon {
            action,
            socket_path,
        } => run_daemon_command(action, &socket_path),
        ParsedCommand::Lifecycle {
            action,
            target,
            options,
        } => {
            let tmux = RealTmux;
            match action {
                LifecycleAction::Start => {
                    let resolved = start_named_target(
                        &tmux,
                        &options.socket_path,
                        &options.config_path,
                        &options.ax_bin,
                        &target,
                    )?;
                    println!("started {:?}", resolved.name);
                }
                LifecycleAction::Stop => {
                    let resolved = stop_named_target(
                        &tmux,
                        &options.socket_path,
                        &options.config_path,
                        &options.ax_bin,
                        &target,
                    )?;
                    println!("stopped {:?}", resolved.name);
                }
                LifecycleAction::Restart => {
                    let resolved = restart_named_target(
                        &tmux,
                        &options.socket_path,
                        &options.config_path,
                        &options.ax_bin,
                        &target,
                    )?;
                    println!("restarted {:?}", resolved.name);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        ParsedCommand::Dispatch {
            target,
            sender,
            fresh,
            options,
        } => {
            let tmux = RealTmux;
            dispatch_runnable_work(
                &tmux,
                &options.socket_path,
                &options.config_path,
                &options.ax_bin,
                &target,
                &sender,
                fresh,
            )?;
            println!("dispatched {:?} from {:?}", target, sender);
            Ok(ExitCode::SUCCESS)
        }
        ParsedCommand::RunAgent {
            runtime,
            workspace,
            socket_path,
            config_path,
            fresh,
            extra_args,
        } => {
            let status = run_with_options(
                &runtime,
                &workspace,
                &socket_path,
                current_exe,
                config_path.as_deref(),
                &LaunchOptions {
                    extra_args,
                    fresh_start: fresh,
                },
            )?;
            Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
        }
        ParsedCommand::Delegate { argv } => delegate_to_go_ax(&argv, current_exe),
    }
}

fn parse_args<I>(args: I, cwd: &Path, current_exe: &Path) -> Result<ParsedCommand, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut argv: Vec<OsString> = args.into_iter().collect();
    if !argv.is_empty() {
        let _ = argv.remove(0);
    }
    let Some(first) = argv.first() else {
        return Ok(ParsedCommand::Help);
    };

    let command = first.to_string_lossy().into_owned();
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        return Ok(ParsedCommand::Help);
    }
    if command == "daemon" {
        return parse_daemon_args(&argv);
    }
    if command == "run-agent" {
        return parse_run_agent_args(&argv, cwd);
    }
    if command == "mcp-server" {
        return Ok(ParsedCommand::Delegate { argv });
    }

    let action = match command.as_str() {
        "start" => Some(LifecycleAction::Start),
        "stop" => Some(LifecycleAction::Stop),
        "restart" => Some(LifecycleAction::Restart),
        "dispatch" => None,
        _ => {
            return Err(CliError::Usage(format!(
                "unknown command {command:?}\n\n{USAGE}"
            )));
        }
    };

    let mut target: Option<String> = None;
    let mut sender: Option<String> = None;
    let mut fresh = false;
    let mut socket_override: Option<PathBuf> = None;
    let mut config_override: Option<PathBuf> = None;
    let mut ax_bin_override: Option<PathBuf> = None;

    let mut i = 1;
    while i < argv.len() {
        let arg = &argv[i];
        match arg.to_string_lossy().as_ref() {
            "-h" | "--help" => return Ok(ParsedCommand::Help),
            "--socket" => {
                i += 1;
                socket_override = Some(parse_socket_path(argv.get(i), "--socket")?);
            }
            "--config" => {
                i += 1;
                config_override = Some(parse_path_arg(argv.get(i), "--config", cwd)?);
            }
            "--ax-bin" => {
                i += 1;
                ax_bin_override = Some(parse_path_arg(argv.get(i), "--ax-bin", cwd)?);
            }
            "--sender" => {
                if action.is_some() {
                    return Err(CliError::Usage(format!(
                        "--sender is only supported by dispatch\n\n{USAGE}"
                    )));
                }
                i += 1;
                sender = Some(parse_string_arg(argv.get(i), "--sender")?);
            }
            "--fresh" => {
                if action.is_some() {
                    return Err(CliError::Usage(format!(
                        "--fresh is only supported by dispatch\n\n{USAGE}"
                    )));
                }
                fresh = true;
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other:?}\n\n{USAGE}"
                )));
            }
            _ => {
                if target.is_some() {
                    return Err(CliError::Usage(format!(
                        "unexpected extra argument {:?}\n\n{USAGE}",
                        arg.to_string_lossy()
                    )));
                }
                target = Some(arg.to_string_lossy().into_owned());
            }
        }
        i += 1;
    }

    let target = target.ok_or_else(|| CliError::Usage(format!("target is required\n\n{USAGE}")))?;
    let options = CommonOptions {
        socket_path: socket_override.unwrap_or_else(|| expand_socket_path(DEFAULT_SOCKET_PATH)),
        config_path: resolve_config_path(config_override, cwd)?,
        ax_bin: ax_bin_override.unwrap_or_else(|| current_exe.to_path_buf()),
    };

    match action {
        Some(action) => Ok(ParsedCommand::Lifecycle {
            action,
            target,
            options,
        }),
        None => Ok(ParsedCommand::Dispatch {
            target,
            sender: sender
                .ok_or_else(|| CliError::Usage(format!("dispatch requires --sender\n\n{USAGE}")))?,
            fresh,
            options,
        }),
    }
}

fn parse_daemon_args(argv: &[OsString]) -> Result<ParsedCommand, CliError> {
    let Some(subcommand) = argv.get(1) else {
        return Ok(ParsedCommand::Help);
    };

    let action = match subcommand.to_string_lossy().as_ref() {
        "-h" | "--help" => return Ok(ParsedCommand::Help),
        "start" => DaemonAction::Start,
        "stop" => DaemonAction::Stop,
        "status" => DaemonAction::Status,
        other => {
            return Err(CliError::Usage(format!(
                "unknown daemon command {other:?}\n\n{USAGE}"
            )));
        }
    };

    let mut socket_path = expand_socket_path(DEFAULT_SOCKET_PATH);
    let mut i = 2;
    while i < argv.len() {
        let arg = &argv[i];
        match arg.to_string_lossy().as_ref() {
            "-h" | "--help" => return Ok(ParsedCommand::Help),
            "--socket" => {
                i += 1;
                socket_path = parse_socket_path(argv.get(i), "--socket")?;
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other:?}\n\n{USAGE}"
                )));
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unexpected extra argument {other:?}\n\n{USAGE}"
                )));
            }
        }
        i += 1;
    }

    Ok(ParsedCommand::Daemon {
        action,
        socket_path,
    })
}

fn parse_run_agent_args(argv: &[OsString], cwd: &Path) -> Result<ParsedCommand, CliError> {
    let mut runtime = "claude".to_owned();
    let mut workspace: Option<String> = None;
    let mut socket_path = expand_socket_path(DEFAULT_SOCKET_PATH);
    let mut config_path: Option<PathBuf> = None;
    let mut fresh = false;
    let mut extra_args = Vec::new();

    let mut i = 1;
    while i < argv.len() {
        let arg = &argv[i];
        match arg.to_string_lossy().as_ref() {
            "-h" | "--help" => return Ok(ParsedCommand::Help),
            "--" => {
                extra_args.extend(
                    argv.iter()
                        .skip(i + 1)
                        .map(|value| value.to_string_lossy().into_owned()),
                );
                break;
            }
            "--runtime" => {
                i += 1;
                runtime = parse_string_arg(argv.get(i), "--runtime")?;
            }
            "--workspace" => {
                i += 1;
                workspace = Some(parse_string_arg(argv.get(i), "--workspace")?);
            }
            "--socket" => {
                i += 1;
                socket_path = parse_socket_path(argv.get(i), "--socket")?;
            }
            "--config" => {
                i += 1;
                config_path = Some(parse_path_arg(argv.get(i), "--config", cwd)?);
            }
            "--fresh" => {
                fresh = true;
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other:?}\n\n{USAGE}"
                )));
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unexpected extra argument {other:?}; use `--` before runtime args\n\n{USAGE}"
                )));
            }
        }
        i += 1;
    }

    Ok(ParsedCommand::RunAgent {
        runtime,
        workspace: workspace
            .ok_or_else(|| CliError::Usage(format!("--workspace is required\n\n{USAGE}")))?,
        socket_path,
        config_path,
        fresh,
        extra_args,
    })
}

fn parse_string_arg(value: Option<&OsString>, flag: &str) -> Result<String, CliError> {
    let Some(value) = value else {
        return Err(CliError::Usage(format!(
            "{flag} requires a value\n\n{USAGE}"
        )));
    };
    Ok(value.to_string_lossy().into_owned())
}

fn parse_path_arg(value: Option<&OsString>, flag: &str, cwd: &Path) -> Result<PathBuf, CliError> {
    let Some(value) = value else {
        return Err(CliError::Usage(format!(
            "{flag} requires a value\n\n{USAGE}"
        )));
    };
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(cwd.join(path))
}

fn parse_socket_path(value: Option<&OsString>, flag: &str) -> Result<PathBuf, CliError> {
    let Some(value) = value else {
        return Err(CliError::Usage(format!(
            "{flag} requires a value\n\n{USAGE}"
        )));
    };
    Ok(expand_socket_path(&value.to_string_lossy()))
}

fn resolve_config_path(config_override: Option<PathBuf>, cwd: &Path) -> Result<PathBuf, CliError> {
    if let Some(path) = config_override {
        return Ok(path);
    }
    find_config_file(cwd).ok_or_else(|| {
        CliError::Usage(format!(
            "no ax config found from {}\n\n{USAGE}",
            cwd.display()
        ))
    })
}

fn run_daemon_command(action: DaemonAction, socket_path: &Path) -> Result<ExitCode, CliError> {
    match action {
        DaemonAction::Start => run_daemon_start(socket_path),
        DaemonAction::Stop => {
            let pid = read_daemon_pid(socket_path)?;
            send_signal(pid, "-TERM")?;
            println!("Sent SIGTERM to daemon (pid {pid})");
            Ok(ExitCode::SUCCESS)
        }
        DaemonAction::Status => {
            match daemon_status(socket_path)? {
                DaemonStatus::Running(pid) => {
                    println!("Daemon: running (pid {pid})");
                    println!("Socket: {}", socket_path.display());
                }
                DaemonStatus::NotRunning => println!("Daemon: not running"),
                DaemonStatus::StalePid => println!("Daemon: not running (stale pid)"),
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run_daemon_start(socket_path: &Path) -> Result<ExitCode, CliError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| DaemonCliError::BuildRuntime { source })?;
    runtime.block_on(run_daemon_until_signal(socket_path.to_path_buf()))?;
    Ok(ExitCode::SUCCESS)
}

async fn run_daemon_until_signal(socket_path: PathBuf) -> Result<(), DaemonCliError> {
    let state_dir = daemon_state_dir(&socket_path)?;
    let pid_path = state_dir.join("daemon.pid");
    let daemon = Daemon::new(socket_path)
        .with_state_dir(&state_dir)
        .map_err(|source| DaemonCliError::LoadState {
            state_dir: state_dir.clone(),
            source,
        })?;
    let handle = daemon.bind().await.map_err(DaemonCliError::Bind)?;
    if let Err(source) = write_pid_file(&pid_path) {
        handle.shutdown().await;
        return Err(DaemonCliError::WritePid {
            path: pid_path,
            source,
        });
    }

    let wait_result = wait_for_shutdown_signal().await;
    handle.shutdown().await;
    if let Err(source) = fs::remove_file(&pid_path) {
        if source.kind() != io::ErrorKind::NotFound {
            eprintln!("remove pid file {:?}: {source}", pid_path);
        }
    }
    wait_result
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() -> Result<(), DaemonCliError> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate =
        signal(SignalKind::terminate()).map_err(|source| DaemonCliError::SignalSetup { source })?;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    tokio::select! {
        result = &mut ctrl_c => result.map_err(|source| DaemonCliError::SignalWait { source }),
        _ = terminate.recv() => Ok(()),
    }
}

fn daemon_state_dir(socket_path: &Path) -> Result<PathBuf, DaemonCliError> {
    socket_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| DaemonCliError::MissingStateDir {
            socket_path: socket_path.to_path_buf(),
        })
}

fn daemon_pid_path(socket_path: &Path) -> Result<PathBuf, DaemonCliError> {
    Ok(daemon_state_dir(socket_path)?.join("daemon.pid"))
}

fn write_pid_file(path: &Path) -> Result<(), io::Error> {
    fs::write(path, std::process::id().to_string())
}

fn read_daemon_pid(socket_path: &Path) -> Result<i32, CliError> {
    let pid_path = daemon_pid_path(socket_path)?;
    let data = fs::read_to_string(&pid_path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            DaemonCliError::MissingPidFile
        } else {
            DaemonCliError::ReadPid {
                path: pid_path.clone(),
                source,
            }
        }
    })?;
    parse_pid(&data).ok_or(DaemonCliError::InvalidPidFile.into())
}

fn parse_pid(raw: &str) -> Option<i32> {
    let pid = raw.trim().parse::<i32>().ok()?;
    (pid > 0).then_some(pid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonStatus {
    Running(i32),
    NotRunning,
    StalePid,
}

fn daemon_status(socket_path: &Path) -> Result<DaemonStatus, CliError> {
    let pid_path = daemon_pid_path(socket_path)?;
    let data = match fs::read_to_string(&pid_path) {
        Ok(data) => data,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(DaemonStatus::NotRunning)
        }
        Err(source) => {
            return Err(DaemonCliError::ReadPid {
                path: pid_path,
                source,
            }
            .into());
        }
    };

    let Some(pid) = parse_pid(&data) else {
        return Ok(DaemonStatus::StalePid);
    };
    if send_signal(pid, "-0")? {
        Ok(DaemonStatus::Running(pid))
    } else {
        Ok(DaemonStatus::StalePid)
    }
}

fn send_signal(pid: i32, signal: &'static str) -> Result<bool, CliError> {
    let output = ProcessCommand::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .output()
        .map_err(|source| DaemonCliError::SignalCommand { signal, source })?;
    if output.status.success() {
        return Ok(true);
    }
    if signal == "-0" {
        return Ok(false);
    }
    Err(DaemonCliError::SignalFailed {
        signal,
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
    .into())
}

fn delegate_to_go_ax(argv: &[OsString], current_exe: &Path) -> Result<ExitCode, CliError> {
    let delegate_bin = env::var_os("AX_GO_BINARY").unwrap_or_else(|| OsString::from("ax"));
    if delegates_to_self(&delegate_bin, current_exe) {
        return Err(CliError::DelegateLoop {
            binary: delegate_bin.to_string_lossy().into_owned(),
        });
    }

    let status = ProcessCommand::new(&delegate_bin)
        .args(argv)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|source| CliError::DelegateLaunch {
            binary: delegate_bin.to_string_lossy().into_owned(),
            source,
        })?;

    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

fn delegates_to_self(delegate_bin: &OsStr, current_exe: &Path) -> bool {
    let candidate = Path::new(delegate_bin);
    if candidate.is_absolute() {
        return candidate == current_exe;
    }
    current_exe
        .file_name()
        .is_some_and(|name| name == delegate_bin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    use tempfile::TempDir;

    fn home_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = home_lock().lock().expect("home lock");
        let old_home = env::var_os("HOME");
        env::set_var("HOME", home);
        let result = f();
        match old_home {
            Some(value) => env::set_var("HOME", value),
            None => env::remove_var("HOME"),
        }
        result
    }

    fn write_config(root: &TempDir) -> PathBuf {
        let config_path = root.path().join(".ax").join("config.yaml");
        fs::create_dir_all(config_path.parent().expect("config dir")).expect("create config dir");
        fs::write(
            &config_path,
            "project: demo\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        )
        .expect("write config");
        config_path
    }

    #[test]
    fn start_defaults_to_discovered_config_and_current_exe() {
        let root = TempDir::new().expect("tempdir");
        let home = TempDir::new().expect("home");
        let _config_path = write_config(&root);
        let cwd = root.path().join("nested");
        fs::create_dir_all(&cwd).expect("create cwd");
        let current_exe = PathBuf::from("/tmp/ax-rs");

        with_home(home.path(), || {
            let parsed = parse_args(
                vec!["ax-rs".into(), "start".into(), "worker".into()],
                &cwd,
                &current_exe,
            )
            .expect("parse");

            assert_eq!(
                parsed,
                ParsedCommand::Lifecycle {
                    action: LifecycleAction::Start,
                    target: "worker".to_owned(),
                    options: CommonOptions {
                        socket_path: expand_socket_path(DEFAULT_SOCKET_PATH),
                        config_path: root.path().join(".ax").join("config.yaml"),
                        ax_bin: current_exe,
                    },
                }
            );
        });
    }

    #[test]
    fn dispatch_accepts_overrides() {
        let cwd = PathBuf::from("/work/project");
        let current_exe = PathBuf::from("/tmp/ax-rs");

        let parsed = parse_args(
            vec![
                "ax-rs".into(),
                "dispatch".into(),
                "worker".into(),
                "--sender".into(),
                "orchestrator".into(),
                "--fresh".into(),
                "--socket".into(),
                "~/daemon.sock".into(),
                "--config".into(),
                "custom.yaml".into(),
                "--ax-bin".into(),
                "./ax-go".into(),
            ],
            &cwd,
            &current_exe,
        )
        .expect("parse");

        assert_eq!(
            parsed,
            ParsedCommand::Dispatch {
                target: "worker".to_owned(),
                sender: "orchestrator".to_owned(),
                fresh: true,
                options: CommonOptions {
                    socket_path: expand_socket_path("~/daemon.sock"),
                    config_path: cwd.join("custom.yaml"),
                    ax_bin: cwd.join("./ax-go"),
                },
            }
        );
    }

    #[test]
    fn run_agent_passthrough_skips_config_resolution() {
        let parsed = parse_args(
            vec![
                "ax-rs".into(),
                "run-agent".into(),
                "--runtime".into(),
                "claude".into(),
                "--workspace".into(),
                "worker".into(),
                "--".into(),
                "--model".into(),
                "gpt-5.4".into(),
            ],
            Path::new("/missing"),
            Path::new("/tmp/ax-rs"),
        )
        .expect("parse");

        assert_eq!(
            parsed,
            ParsedCommand::RunAgent {
                runtime: "claude".to_owned(),
                workspace: "worker".to_owned(),
                socket_path: expand_socket_path(DEFAULT_SOCKET_PATH),
                config_path: None,
                fresh: false,
                extra_args: vec!["--model".to_owned(), "gpt-5.4".to_owned()],
            }
        );
    }

    #[test]
    fn dispatch_requires_sender() {
        let root = TempDir::new().expect("tempdir");
        let home = TempDir::new().expect("home");
        let _config_path = write_config(&root);

        with_home(home.path(), || {
            let err = parse_args(
                vec!["ax-rs".into(), "dispatch".into(), "worker".into()],
                root.path(),
                Path::new("/tmp/ax-rs"),
            )
            .expect_err("missing sender should fail");
            assert_eq!(
                err.to_string(),
                format!("dispatch requires --sender\n\n{USAGE}")
            );
        });
    }

    #[test]
    fn run_agent_parses_flags_and_extra_args() {
        let parsed = parse_args(
            vec![
                "ax-rs".into(),
                "run-agent".into(),
                "--runtime".into(),
                "codex".into(),
                "--workspace".into(),
                "worker".into(),
                "--socket".into(),
                "~/daemon.sock".into(),
                "--config".into(),
                "ax.yaml".into(),
                "--fresh".into(),
                "--".into(),
                "--model".into(),
                "gpt-5.4".into(),
            ],
            Path::new("/repo"),
            Path::new("/tmp/ax-rs"),
        )
        .expect("parse");

        assert_eq!(
            parsed,
            ParsedCommand::RunAgent {
                runtime: "codex".to_owned(),
                workspace: "worker".to_owned(),
                socket_path: expand_socket_path("~/daemon.sock"),
                config_path: Some(PathBuf::from("/repo").join("ax.yaml")),
                fresh: true,
                extra_args: vec!["--model".to_owned(), "gpt-5.4".to_owned()],
            }
        );
    }

    #[test]
    fn daemon_parses_socket_override_without_config_resolution() {
        let parsed = parse_args(
            vec![
                "ax-rs".into(),
                "daemon".into(),
                "status".into(),
                "--socket".into(),
                "~/daemon.sock".into(),
            ],
            Path::new("/missing"),
            Path::new("/tmp/ax-rs"),
        )
        .expect("parse");

        assert_eq!(
            parsed,
            ParsedCommand::Daemon {
                action: DaemonAction::Status,
                socket_path: expand_socket_path("~/daemon.sock"),
            }
        );
    }

    #[test]
    fn parse_pid_rejects_invalid_values() {
        assert_eq!(parse_pid("12345\n"), Some(12_345));
        assert_eq!(parse_pid("0"), None);
        assert_eq!(parse_pid("-7"), None);
        assert_eq!(parse_pid("abc"), None);
    }
}
