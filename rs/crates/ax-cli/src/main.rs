#![forbid(unsafe_code)]

mod daemon_client;

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ax_agent::{run_with_options, LaunchOptions};
use ax_config::{find_config_file, Config};
use ax_daemon::{expand_socket_path, Daemon, DEFAULT_SOCKET_PATH};
use ax_proto::types::Message;
use ax_tmux::{attach_session, create_ephemeral_session, session_exists};
use ax_workspace::{
    cleanup_orchestrator_state, dispatch_runnable_work, ensure_artifacts, ensure_orchestrator_tree,
    orchestrator_name, remove_mcp_config, restart_named_target, root_orchestrator_dir,
    start_named_target, stop_named_target, Manager, RealTmux, TmuxBackend,
};
use daemon_client::{DaemonClient, DaemonClientError};

const USAGE: &str = "\
ax-rs - thin Rust entrypoint for migrated workspace control

Usage:
  ax-rs daemon start [--socket PATH]
  ax-rs daemon stop [--socket PATH]
  ax-rs daemon status [--socket PATH]
  ax-rs up [--config PATH] [--socket PATH] [--ax-bin PATH]
  ax-rs down [--config PATH] [--socket PATH] [--ax-bin PATH]
  ax-rs claude [claude args...]
  ax-rs codex [codex args...]
  ax-rs send [--config PATH] [--socket PATH] <workspace> <message...>
  ax-rs messages [--from NAME] [--limit N] [--wait] [--timeout SECONDS] [--json] [--socket PATH]
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

const ROOT_ORCHESTRATOR_FAILURE_HOLD_SCRIPT: &str = "\"$@\"\nstatus=$?\nif [ \"$status\" -ne 0 ] && [ \"$status\" -ne 130 ] && [ \"$status\" -ne 143 ]; then\n  printf '\\n[ax] Root orchestrator process exited unexpectedly with status %s.\\n' \"$status\"\n  printf '[ax] Common causes: runtime binary not found, auth/config issues, or a CLI crash.\\n'\n  printf '[ax] Press Enter to close this tmux session.\\n'\n  IFS= read -r _\nfi\nexit \"$status\"";
const CLI_INBOX_WORKSPACE: &str = "_cli";

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
    Up {
        options: CommonOptions,
    },
    Down {
        options: CommonOptions,
    },
    RootOrchestrator {
        runtime: String,
        passthrough_args: Vec<String>,
        options: CommonOptions,
    },
    Send {
        to: String,
        message: String,
        socket_path: PathBuf,
        config_path: PathBuf,
    },
    Messages {
        from: String,
        limit: i64,
        wait: bool,
        timeout_seconds: u64,
        json_output: bool,
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
    Up(UpCliError),
    Down(DownCliError),
    RootOrchestrator(RootOrchestratorCliError),
    Send(SendCliError),
    Messages(MessagesCliError),
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

#[derive(Debug)]
enum UpCliError {
    LoadConfig(ax_config::TreeError),
    LoadTree(ax_config::TreeError),
    PrepareWorkspace {
        name: String,
        source: ax_workspace::WorkspaceError,
    },
    PrepareOrchestrators(ax_workspace::OrchestratorError),
    StartDaemonProcess {
        source: io::Error,
    },
    PollDaemonProcess {
        source: io::Error,
    },
    DaemonDidNotStart {
        socket_path: PathBuf,
    },
}

#[derive(Debug)]
enum DownCliError {
    LoadConfig(ax_config::TreeError),
    StopWorkspace {
        name: String,
        source: ax_workspace::WorkspaceError,
    },
    StopOrchestrator {
        name: String,
        source: ax_tmux::TmuxError,
    },
    CleanupRootOrchestrator(ax_workspace::OrchestratorError),
    RemoveConfigMcp(ax_workspace::McpConfigError),
}

#[derive(Debug)]
enum RootOrchestratorCliError {
    UnsupportedRuntime(String),
    LoadTree(ax_config::TreeError),
    PrepareOrchestrators(ax_workspace::OrchestratorError),
    ResolveRootDir(ax_workspace::OrchestratorError),
    StartDaemon(String),
    CreateSession(ax_tmux::TmuxError),
    AttachSession(ax_tmux::TmuxError),
}

#[derive(Debug)]
enum SendCliError {
    Connect(DaemonClientError),
    Send(DaemonClientError),
}

#[derive(Debug)]
enum MessagesCliError {
    Connect(DaemonClientError),
    Read(DaemonClientError),
    FormatJson(serde_json::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            Self::Daemon(source) => write!(f, "{source}"),
            Self::Up(source) => write!(f, "{source}"),
            Self::Down(source) => write!(f, "{source}"),
            Self::RootOrchestrator(source) => write!(f, "{source}"),
            Self::Send(source) => write!(f, "{source}"),
            Self::Messages(source) => write!(f, "{source}"),
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

impl fmt::Display for SendCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(source) => write!(f, "connect to daemon: {source} (is daemon running?)"),
            Self::Send(source) => write!(f, "send: {source}"),
        }
    }
}

impl fmt::Display for MessagesCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(source) => write!(f, "connect to daemon: {source} (is daemon running?)"),
            Self::Read(source) => write!(f, "read messages: {source}"),
            Self::FormatJson(source) => write!(f, "format messages as json: {source}"),
        }
    }
}

impl fmt::Display for RootOrchestratorCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRuntime(runtime) => write!(f, "unsupported runtime {runtime:?}"),
            Self::LoadTree(source) => write!(f, "load config tree: {source}"),
            #[allow(clippy::match_same_arms)]
            Self::PrepareOrchestrators(source) => write!(f, "{source}"),
            Self::ResolveRootDir(source) => write!(f, "{source}"),
            Self::StartDaemon(source) => write!(f, "start daemon: {source}"),
            Self::CreateSession(source) => write!(f, "create orchestrator session: {source}"),
            Self::AttachSession(source) => write!(f, "{source}"),
        }
    }
}

impl fmt::Display for DownCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadConfig(source) => write!(f, "{source}"),
            Self::StopWorkspace { name, source } => {
                write!(f, "destroy workspace {name:?}: {source}")
            }
            Self::StopOrchestrator { name, source } => {
                write!(f, "destroy orchestrator session {name:?}: {source}")
            }
            Self::CleanupRootOrchestrator(source) => write!(f, "{source}"),
            Self::RemoveConfigMcp(source) => write!(f, "{source}"),
        }
    }
}

impl fmt::Display for UpCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadConfig(source) => write!(f, "{source}"),
            Self::LoadTree(source) => write!(f, "load config tree: {source}"),
            Self::PrepareWorkspace { name, source } => {
                write!(f, "prepare workspace {name:?}: {source}")
            }
            Self::PrepareOrchestrators(source) => write!(f, "{source}"),
            Self::StartDaemonProcess { source } => write!(f, "start daemon process: {source}"),
            Self::PollDaemonProcess { source } => write!(f, "poll daemon process: {source}"),
            Self::DaemonDidNotStart { socket_path } => {
                write!(
                    f,
                    "daemon did not start within 3s ({})",
                    socket_path.display()
                )
            }
        }
    }
}

impl fmt::Display for DaemonCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingStateDir { socket_path } => {
                write!(f, "resolve daemon state dir from socket {socket_path:?}")
            }
            Self::BuildRuntime { source } => write!(f, "build tokio runtime: {source}"),
            Self::LoadState { state_dir, source } => {
                write!(f, "load daemon state from {state_dir:?}: {source}")
            }
            Self::Bind(source) => write!(f, "{source}"),
            Self::SignalSetup { source } => write!(f, "install shutdown signal handler: {source}"),
            Self::SignalWait { source } => write!(f, "wait for shutdown signal: {source}"),
            Self::WritePid { path, source } => write!(f, "write pid file {path:?}: {source}"),
            Self::ReadPid { path, source } => write!(f, "read pid file {path:?}: {source}"),
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

impl From<UpCliError> for CliError {
    fn from(source: UpCliError) -> Self {
        Self::Up(source)
    }
}

impl From<DownCliError> for CliError {
    fn from(source: DownCliError) -> Self {
        Self::Down(source)
    }
}

impl From<RootOrchestratorCliError> for CliError {
    fn from(source: RootOrchestratorCliError) -> Self {
        Self::RootOrchestrator(source)
    }
}

impl From<SendCliError> for CliError {
    fn from(source: SendCliError) -> Self {
        Self::Send(source)
    }
}

impl From<MessagesCliError> for CliError {
    fn from(source: MessagesCliError) -> Self {
        Self::Messages(source)
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
        ParsedCommand::Up { options } => run_up(&options, current_exe),
        ParsedCommand::Down { options } => run_down(&options),
        ParsedCommand::RootOrchestrator {
            runtime,
            passthrough_args,
            options,
        } => run_root_orchestrator(&runtime, &passthrough_args, &options, current_exe),
        ParsedCommand::Send {
            to,
            message,
            socket_path,
            config_path,
        } => run_send(&to, &message, &socket_path, &config_path),
        ParsedCommand::Messages {
            from,
            limit,
            wait,
            timeout_seconds,
            json_output,
            socket_path,
        } => run_messages(
            &from,
            limit,
            wait,
            timeout_seconds,
            json_output,
            &socket_path,
        ),
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
            println!("dispatched {target:?} from {sender:?}");
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
    let mut tail: Vec<OsString> = args.into_iter().collect();
    if !tail.is_empty() {
        let _ = tail.remove(0);
    }
    let Some(first) = tail.first() else {
        return Ok(ParsedCommand::Help);
    };

    let command = first.to_string_lossy().into_owned();
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        return Ok(ParsedCommand::Help);
    }
    if command == "daemon" {
        return parse_daemon_args(&tail);
    }
    if command == "up" {
        return parse_up_args(&tail, cwd, current_exe);
    }
    if command == "down" {
        return parse_down_args(&tail, cwd, current_exe);
    }
    if matches!(command.as_str(), "claude" | "codex") {
        return parse_root_orchestrator_args(&command, &tail, cwd, current_exe);
    }
    if command == "send" {
        return parse_send_args(&tail, cwd);
    }
    if matches!(command.as_str(), "messages" | "messages-json" | "msg") {
        return parse_messages_args(&tail, matches!(command.as_str(), "messages-json"));
    }
    if command == "run-agent" {
        return parse_run_agent_args(&tail, cwd);
    }
    if command == "mcp-server" {
        return Ok(ParsedCommand::Delegate { argv: tail });
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
    while i < tail.len() {
        let arg = &tail[i];
        match arg.to_string_lossy().as_ref() {
            "-h" | "--help" => return Ok(ParsedCommand::Help),
            "--socket" => {
                i += 1;
                socket_override = Some(parse_socket_path(tail.get(i), "--socket")?);
            }
            "--config" => {
                i += 1;
                config_override = Some(parse_path_arg(tail.get(i), "--config", cwd)?);
            }
            "--ax-bin" => {
                i += 1;
                ax_bin_override = Some(parse_path_arg(tail.get(i), "--ax-bin", cwd)?);
            }
            "--sender" => {
                if action.is_some() {
                    return Err(CliError::Usage(format!(
                        "--sender is only supported by dispatch\n\n{USAGE}"
                    )));
                }
                i += 1;
                sender = Some(parse_string_arg(tail.get(i), "--sender")?);
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

fn parse_up_args(
    argv: &[OsString],
    cwd: &Path,
    current_exe: &Path,
) -> Result<ParsedCommand, CliError> {
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

    Ok(ParsedCommand::Up {
        options: CommonOptions {
            socket_path: socket_override.unwrap_or_else(|| expand_socket_path(DEFAULT_SOCKET_PATH)),
            config_path: resolve_config_path(config_override, cwd)?,
            ax_bin: ax_bin_override.unwrap_or_else(|| current_exe.to_path_buf()),
        },
    })
}

fn parse_down_args(
    argv: &[OsString],
    cwd: &Path,
    current_exe: &Path,
) -> Result<ParsedCommand, CliError> {
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

    Ok(ParsedCommand::Down {
        options: CommonOptions {
            socket_path: socket_override.unwrap_or_else(|| expand_socket_path(DEFAULT_SOCKET_PATH)),
            config_path: resolve_config_path(config_override, cwd)?,
            ax_bin: ax_bin_override.unwrap_or_else(|| current_exe.to_path_buf()),
        },
    })
}

fn parse_root_orchestrator_args(
    command: &str,
    argv: &[OsString],
    cwd: &Path,
    current_exe: &Path,
) -> Result<ParsedCommand, CliError> {
    let passthrough_args: Vec<String> = argv
        .iter()
        .skip(1)
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    let passthrough_args = if command == "claude" {
        normalize_claude_passthrough_args(&passthrough_args)
    } else {
        passthrough_args
    };

    Ok(ParsedCommand::RootOrchestrator {
        runtime: command.to_owned(),
        passthrough_args,
        options: CommonOptions {
            socket_path: expand_socket_path(DEFAULT_SOCKET_PATH),
            config_path: resolve_config_path(None, cwd)?,
            ax_bin: current_exe.to_path_buf(),
        },
    })
}

fn parse_send_args(argv: &[OsString], cwd: &Path) -> Result<ParsedCommand, CliError> {
    let mut socket_path = expand_socket_path(DEFAULT_SOCKET_PATH);
    let mut config_override: Option<PathBuf> = None;
    let mut to: Option<String> = None;
    let mut message_parts = Vec::new();

    let mut i = 1;
    while i < argv.len() {
        let arg = &argv[i];
        if to.is_none() {
            match arg.to_string_lossy().as_ref() {
                "-h" | "--help" => return Ok(ParsedCommand::Help),
                "--socket" => {
                    i += 1;
                    socket_path = parse_socket_path(argv.get(i), "--socket")?;
                }
                "--config" => {
                    i += 1;
                    config_override = Some(parse_path_arg(argv.get(i), "--config", cwd)?);
                }
                other if other.starts_with('-') => {
                    return Err(CliError::Usage(format!(
                        "unknown flag {other:?}\n\n{USAGE}"
                    )));
                }
                _ => {
                    to = Some(arg.to_string_lossy().into_owned());
                }
            }
        } else {
            message_parts.push(arg.to_string_lossy().into_owned());
        }
        i += 1;
    }

    let to =
        to.ok_or_else(|| CliError::Usage(format!("send requires a workspace target\n\n{USAGE}")))?;
    if message_parts.is_empty() {
        return Err(CliError::Usage(format!(
            "send requires a message body\n\n{USAGE}"
        )));
    }

    Ok(ParsedCommand::Send {
        to,
        message: message_parts.join(" "),
        socket_path,
        config_path: resolve_config_path(config_override, cwd)?,
    })
}

fn parse_messages_args(argv: &[OsString], force_json: bool) -> Result<ParsedCommand, CliError> {
    let mut socket_path = expand_socket_path(DEFAULT_SOCKET_PATH);
    let mut from = String::new();
    let mut limit = 10_i64;
    let mut wait = false;
    let mut timeout_seconds = 120_u64;
    let mut json_output = force_json;

    let mut i = 1;
    while i < argv.len() {
        let arg = &argv[i];
        match arg.to_string_lossy().as_ref() {
            "-h" | "--help" => return Ok(ParsedCommand::Help),
            "--socket" => {
                i += 1;
                socket_path = parse_socket_path(argv.get(i), "--socket")?;
            }
            "--from" => {
                i += 1;
                from = parse_string_arg(argv.get(i), "--from")?;
            }
            "--limit" => {
                i += 1;
                limit = parse_i64_arg(argv.get(i), "--limit")?;
            }
            "--wait" => {
                wait = true;
            }
            "--timeout" => {
                i += 1;
                timeout_seconds = parse_u64_arg(argv.get(i), "--timeout")?;
            }
            "--json" => {
                json_output = true;
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

    Ok(ParsedCommand::Messages {
        from,
        limit,
        wait,
        timeout_seconds,
        json_output,
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

fn parse_i64_arg(value: Option<&OsString>, flag: &str) -> Result<i64, CliError> {
    let raw = parse_string_arg(value, flag)?;
    raw.parse::<i64>()
        .map_err(|_| CliError::Usage(format!("{flag} requires an integer value\n\n{USAGE}")))
}

fn parse_u64_arg(value: Option<&OsString>, flag: &str) -> Result<u64, CliError> {
    let raw = parse_string_arg(value, flag)?;
    raw.parse::<u64>().map_err(|_| {
        CliError::Usage(format!(
            "{flag} requires a non-negative integer value\n\n{USAGE}"
        ))
    })
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

fn run_send(
    to: &str,
    message: &str,
    socket_path: &Path,
    config_path: &Path,
) -> Result<ExitCode, CliError> {
    let mut client =
        DaemonClient::connect(socket_path, "orchestrator").map_err(SendCliError::Connect)?;
    let result = client
        .send_message(to, message, Some(config_path))
        .map_err(SendCliError::Send)?;

    println!("Message sent to {:?} (id: {})", to, result.message_id);
    println!("Agent {to:?} readied for queued work.");
    Ok(ExitCode::SUCCESS)
}

fn run_messages(
    from: &str,
    limit: i64,
    wait: bool,
    timeout_seconds: u64,
    json_output: bool,
    socket_path: &Path,
) -> Result<ExitCode, CliError> {
    let mut client = DaemonClient::connect(socket_path, CLI_INBOX_WORKSPACE)
        .map_err(MessagesCliError::Connect)?;
    if !wait {
        let messages = client
            .read_messages(limit, from)
            .map_err(MessagesCliError::Read)?;
        print!(
            "{}",
            format_messages_output(&messages, json_output).map_err(MessagesCliError::FormatJson)?
        );
        return Ok(ExitCode::SUCCESS);
    }

    println!("Waiting for CLI inbox messages for `{CLI_INBOX_WORKSPACE}`... (Ctrl+C to stop)");
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    while Instant::now() < deadline {
        let messages = client
            .read_messages(limit, from)
            .map_err(MessagesCliError::Read)?;
        if !messages.is_empty() {
            print!(
                "{}",
                format_messages_output(&messages, json_output)
                    .map_err(MessagesCliError::FormatJson)?
            );
            return Ok(ExitCode::SUCCESS);
        }
        thread::sleep(Duration::from_secs(2));
    }

    print!("{}", timeout_messages_output(json_output));
    Ok(ExitCode::SUCCESS)
}

fn run_up(options: &CommonOptions, current_exe: &Path) -> Result<ExitCode, CliError> {
    let cfg = Config::load(&options.config_path).map_err(UpCliError::LoadConfig)?;
    let tree = Config::load_tree(&options.config_path).map_err(UpCliError::LoadTree)?;

    println!("Project: {}", cfg.project);
    println!();

    ensure_daemon_running(&options.socket_path, current_exe)?;
    println!("Daemon: running");

    println!();
    println!("Workspaces:");
    for (name, workspace) in &cfg.workspaces {
        ensure_artifacts(
            name,
            workspace,
            &options.socket_path,
            Some(&options.config_path),
            &options.ax_bin,
        )
        .map_err(|source| UpCliError::PrepareWorkspace {
            name: name.clone(),
            source,
        })?;
        println!("  {name}: ready (on-demand, dir: {})", workspace.dir);
    }

    println!();
    println!("Orchestrators:");
    let skip_root =
        reconcile_root_orchestrator_state(&tree).map_err(UpCliError::PrepareOrchestrators)?;
    ensure_orchestrator_tree(
        &RealTmux,
        &tree,
        &options.socket_path,
        Some(&options.config_path),
        &options.ax_bin,
        false,
        skip_root,
    )
    .map_err(UpCliError::PrepareOrchestrators)?;
    println!("  tree: ready (on-demand)");

    if skip_root {
        println!();
        println!("Managed root orchestrator state is disabled by config.");
        println!("Workspace and child/project orchestrator agents will start on demand when work is dispatched.");
        println!(
            "Run 'ax claude' or 'ax codex' to launch a foreground root orchestrator manually."
        );
        return Ok(ExitCode::SUCCESS);
    }

    println!();
    println!("Run 'ax claude' or 'ax codex' to launch the root orchestrator CLI.");
    println!("Workspace and child/project orchestrator agents will start on demand when messages or tasks are dispatched.");
    Ok(ExitCode::SUCCESS)
}

fn run_down(options: &CommonOptions) -> Result<ExitCode, CliError> {
    let cfg = Config::load(&options.config_path).map_err(DownCliError::LoadConfig)?;

    println!("Stopping workspaces:");
    let manager = Manager::new(
        options.socket_path.clone(),
        Some(options.config_path.clone()),
        options.ax_bin.clone(),
    );
    for (name, workspace) in &cfg.workspaces {
        if !RealTmux.session_exists(name) {
            println!("  {name}: not running (skipped)");
            continue;
        }
        manager
            .destroy(name, &workspace.dir)
            .map_err(|source| DownCliError::StopWorkspace {
                name: name.clone(),
                source,
            })?;
        println!("  {name}: stopped");
    }

    if let Ok(tree) = Config::load_tree(&options.config_path) {
        println!();
        println!("Stopping orchestrators:");
        stop_orchestrator_sessions(&RealTmux, &tree)?;
        let _ = reconcile_root_orchestrator_state(&tree)
            .map_err(DownCliError::CleanupRootOrchestrator)?;
    }

    if let Some(config_dir) = options.config_path.parent() {
        remove_mcp_config(config_dir).map_err(DownCliError::RemoveConfigMcp)?;
    }

    println!();
    match daemon_status(&options.socket_path)? {
        DaemonStatus::Running(pid) => {
            send_signal(pid, "-TERM")?;
            println!("Daemon: stopped");
        }
        DaemonStatus::NotRunning | DaemonStatus::StalePid => {
            println!("Daemon: not running");
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn run_root_orchestrator(
    runtime_name: &str,
    passthrough_args: &[String],
    options: &CommonOptions,
    current_exe: &Path,
) -> Result<ExitCode, CliError> {
    let runtime = ax_agent::Runtime::normalize(runtime_name)
        .ok_or_else(|| RootOrchestratorCliError::UnsupportedRuntime(runtime_name.to_owned()))?;
    let mut tree =
        Config::load_tree(&options.config_path).map_err(RootOrchestratorCliError::LoadTree)?;

    if tree.disable_root_orchestrator {
        reconcile_root_orchestrator_state(&tree)
            .map_err(RootOrchestratorCliError::PrepareOrchestrators)?;
    }

    runtime.as_str().clone_into(&mut tree.orchestrator_runtime);
    ensure_daemon_running(&options.socket_path, current_exe)
        .map_err(|err| RootOrchestratorCliError::StartDaemon(err.to_string()))?;
    ensure_orchestrator_tree(
        &RealTmux,
        &tree,
        &options.socket_path,
        Some(&options.config_path),
        &options.ax_bin,
        true,
        false,
    )
    .map_err(RootOrchestratorCliError::PrepareOrchestrators)?;

    let self_name = orchestrator_name(&tree.prefix);
    if session_exists(&self_name) {
        attach_session(&self_name).map_err(RootOrchestratorCliError::AttachSession)?;
        return Ok(ExitCode::SUCCESS);
    }

    let orch_dir = root_orchestrator_dir().map_err(RootOrchestratorCliError::ResolveRootDir)?;
    let mut argv = vec![
        options.ax_bin.display().to_string(),
        "run-agent".to_owned(),
        "--runtime".to_owned(),
        runtime.as_str().to_owned(),
        "--workspace".to_owned(),
        self_name.clone(),
        "--socket".to_owned(),
        options.socket_path.display().to_string(),
        "--config".to_owned(),
        options.config_path.display().to_string(),
    ];
    if !passthrough_args.is_empty() {
        argv.push("--".to_owned());
        argv.extend_from_slice(passthrough_args);
    }
    let wrapped = wrap_root_orchestrator_ephemeral_argv(&argv);
    let refs: Vec<&str> = wrapped.iter().map(String::as_str).collect();
    create_ephemeral_session(&self_name, &orch_dir.display().to_string(), &refs)
        .map_err(RootOrchestratorCliError::CreateSession)?;
    attach_session(&self_name).map_err(RootOrchestratorCliError::AttachSession)?;
    Ok(ExitCode::SUCCESS)
}

fn reconcile_root_orchestrator_state(
    tree: &ax_config::ProjectNode,
) -> Result<bool, ax_workspace::OrchestratorError> {
    if !tree.disable_root_orchestrator {
        return Ok(false);
    }
    let orch_dir = root_orchestrator_dir()?;
    cleanup_orchestrator_state(&RealTmux, &orchestrator_name(""), &orch_dir)?;
    Ok(true)
}

fn stop_orchestrator_sessions<B: TmuxBackend>(
    tmux: &B,
    tree: &ax_config::ProjectNode,
) -> Result<(), DownCliError> {
    for child in &tree.children {
        stop_orchestrator_sessions(tmux, child)?;
    }

    let name = orchestrator_name(&tree.prefix);
    if tmux.session_exists(&name) {
        tmux.destroy_session(&name)
            .map_err(|source| DownCliError::StopOrchestrator {
                name: name.clone(),
                source,
            })?;
        println!("  {name}: stopped");
    }
    Ok(())
}

fn normalize_claude_passthrough_args(args: &[String]) -> Vec<String> {
    if args.is_empty() {
        return Vec::new();
    }
    let mut normalized = args.to_vec();
    match normalized.first().map(String::as_str) {
        Some("resume") => "--resume".clone_into(&mut normalized[0]),
        Some("continue") => "--continue".clone_into(&mut normalized[0]),
        _ => {}
    }
    normalized
}

fn wrap_root_orchestrator_ephemeral_argv(argv: &[String]) -> Vec<String> {
    if argv.is_empty() {
        return Vec::new();
    }
    let mut wrapped = vec![
        "sh".to_owned(),
        "-lc".to_owned(),
        ROOT_ORCHESTRATOR_FAILURE_HOLD_SCRIPT.to_owned(),
        "ax-root-orchestrator".to_owned(),
    ];
    wrapped.extend_from_slice(argv);
    wrapped
}

fn ensure_daemon_running(socket_path: &Path, current_exe: &Path) -> Result<(), CliError> {
    if matches!(daemon_status(socket_path)?, DaemonStatus::Running(_)) {
        return Ok(());
    }

    let mut daemon = ProcessCommand::new(current_exe);
    daemon
        .arg("daemon")
        .arg("start")
        .arg("--socket")
        .arg(socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = daemon
        .spawn()
        .map_err(|source| UpCliError::StartDaemonProcess { source })?;
    for _ in 0..30 {
        if socket_path.exists() {
            return Ok(());
        }
        if child
            .try_wait()
            .map_err(|source| UpCliError::PollDaemonProcess { source })?
            .is_some()
        {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    Err(UpCliError::DaemonDidNotStart {
        socket_path: socket_path.to_path_buf(),
    }
    .into())
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
            eprintln!("remove pid file {pid_path:?}: {source}");
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

fn format_messages_output(
    messages: &[Message],
    json_output: bool,
) -> Result<String, serde_json::Error> {
    if json_output {
        return serde_json::to_string_pretty(messages).map(|text| format!("{text}\n"));
    }
    if messages.is_empty() {
        return Ok("No messages.\n".to_owned());
    }

    let mut out = String::new();
    for message in messages {
        out.push_str(&format!(
            "── [{}] from {} ──\n{}\n\n",
            message.created_at.format("%H:%M:%S"),
            message.from,
            message.content
        ));
    }
    Ok(out)
}

fn timeout_messages_output(json_output: bool) -> &'static str {
    if json_output {
        "[]\n"
    } else {
        "No messages received within timeout.\n"
    }
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
    fn up_defaults_to_discovered_config_and_current_exe() {
        let root = TempDir::new().expect("tempdir");
        let home = TempDir::new().expect("home");
        let _config_path = write_config(&root);
        let cwd = root.path().join("nested");
        fs::create_dir_all(&cwd).expect("create cwd");
        let current_exe = PathBuf::from("/tmp/ax-rs");

        with_home(home.path(), || {
            let parsed =
                parse_args(vec!["ax-rs".into(), "up".into()], &cwd, &current_exe).expect("parse");

            assert_eq!(
                parsed,
                ParsedCommand::Up {
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
    fn down_defaults_to_discovered_config_and_current_exe() {
        let root = TempDir::new().expect("tempdir");
        let home = TempDir::new().expect("home");
        let _config_path = write_config(&root);
        let cwd = root.path().join("nested");
        fs::create_dir_all(&cwd).expect("create cwd");
        let current_exe = PathBuf::from("/tmp/ax-rs");

        with_home(home.path(), || {
            let parsed =
                parse_args(vec!["ax-rs".into(), "down".into()], &cwd, &current_exe).expect("parse");

            assert_eq!(
                parsed,
                ParsedCommand::Down {
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
    fn claude_parses_passthrough_args_and_normalizes_resume_alias() {
        let root = TempDir::new().expect("tempdir");
        let home = TempDir::new().expect("home");
        let _config_path = write_config(&root);
        let cwd = root.path().join("nested");
        fs::create_dir_all(&cwd).expect("create cwd");
        let current_exe = PathBuf::from("/tmp/ax-rs");

        with_home(home.path(), || {
            let parsed = parse_args(
                vec![
                    "ax-rs".into(),
                    "claude".into(),
                    "resume".into(),
                    "--model".into(),
                    "sonnet".into(),
                ],
                &cwd,
                &current_exe,
            )
            .expect("parse");

            assert_eq!(
                parsed,
                ParsedCommand::RootOrchestrator {
                    runtime: "claude".to_owned(),
                    passthrough_args: vec![
                        "--resume".to_owned(),
                        "--model".to_owned(),
                        "sonnet".to_owned(),
                    ],
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
    fn wrap_root_orchestrator_ephemeral_argv_preserves_original_command() {
        let argv = vec![
            "ax-rs".to_owned(),
            "run-agent".to_owned(),
            "--runtime".to_owned(),
            "codex".to_owned(),
            "--workspace".to_owned(),
            "orchestrator".to_owned(),
        ];

        let wrapped = wrap_root_orchestrator_ephemeral_argv(&argv);
        assert_eq!(wrapped.len(), argv.len() + 4);
        assert_eq!(&wrapped[0], "sh");
        assert_eq!(&wrapped[1], "-lc");
        assert!(wrapped[2].contains("Root orchestrator process exited unexpectedly"));
        assert_eq!(&wrapped[3], "ax-root-orchestrator");
        assert_eq!(&wrapped[4..], &argv);
    }

    #[test]
    fn wrap_root_orchestrator_ephemeral_argv_handles_empty_input() {
        assert!(wrap_root_orchestrator_ephemeral_argv(&[]).is_empty());
    }

    #[test]
    fn send_defaults_to_discovered_config() {
        let root = TempDir::new().expect("tempdir");
        let home = TempDir::new().expect("home");
        let _config_path = write_config(&root);
        let cwd = root.path().join("nested");
        fs::create_dir_all(&cwd).expect("create cwd");

        with_home(home.path(), || {
            let parsed = parse_args(
                vec![
                    "ax-rs".into(),
                    "send".into(),
                    "worker".into(),
                    "hello".into(),
                    "world".into(),
                ],
                &cwd,
                Path::new("/tmp/ax-rs"),
            )
            .expect("parse");

            assert_eq!(
                parsed,
                ParsedCommand::Send {
                    to: "worker".to_owned(),
                    message: "hello world".to_owned(),
                    socket_path: expand_socket_path(DEFAULT_SOCKET_PATH),
                    config_path: root.path().join(".ax").join("config.yaml"),
                }
            );
        });
    }

    #[test]
    fn messages_parses_filters_and_json_alias() {
        let parsed = parse_args(
            vec![
                "ax-rs".into(),
                "messages-json".into(),
                "--from".into(),
                "worker".into(),
                "--limit".into(),
                "5".into(),
                "--wait".into(),
                "--timeout".into(),
                "30".into(),
                "--socket".into(),
                "~/daemon.sock".into(),
            ],
            Path::new("/missing"),
            Path::new("/tmp/ax-rs"),
        )
        .expect("parse");

        assert_eq!(
            parsed,
            ParsedCommand::Messages {
                from: "worker".to_owned(),
                limit: 5,
                wait: true,
                timeout_seconds: 30,
                json_output: true,
                socket_path: expand_socket_path("~/daemon.sock"),
            }
        );
    }

    #[test]
    fn format_messages_output_text_and_json_match_go_shape() {
        let messages = vec![Message {
            id: "msg-1".to_owned(),
            from: "ax.orchestrator".to_owned(),
            to: CLI_INBOX_WORKSPACE.to_owned(),
            content: "Task ready".to_owned(),
            task_id: String::new(),
            created_at: "2026-04-14T02:30:00Z".parse().expect("timestamp"),
        }];

        let text = format_messages_output(&messages, false).expect("text output");
        assert!(text.contains("── [02:30:00] from ax.orchestrator ──"));
        assert!(text.contains("Task ready"));

        let json = format_messages_output(&messages, true).expect("json output");
        let decoded: Vec<Message> = serde_json::from_str(&json).expect("decode json");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].id, "msg-1");
        assert_eq!(decoded[0].to, CLI_INBOX_WORKSPACE);
    }

    #[test]
    fn timeout_messages_output_matches_text_and_json_modes() {
        assert_eq!(timeout_messages_output(true), "[]\n");
        assert_eq!(
            timeout_messages_output(false),
            "No messages received within timeout.\n"
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
