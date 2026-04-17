#![forbid(unsafe_code)]

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};

use ax_agent::{run_with_options, LaunchOptions};
use ax_config::find_config_file;
use ax_daemon::{expand_socket_path, DEFAULT_SOCKET_PATH};
use ax_workspace::{
    dispatch_runnable_work, restart_named_target, start_named_target, stop_named_target, RealTmux,
};

const USAGE: &str = "\
ax-rs - thin Rust entrypoint for migrated workspace control

Usage:
  ax-rs start <target> [--config PATH] [--socket PATH] [--ax-bin PATH]
  ax-rs stop <target> [--config PATH] [--socket PATH] [--ax-bin PATH]
  ax-rs restart <target> [--config PATH] [--socket PATH] [--ax-bin PATH]
  ax-rs dispatch <target> --sender NAME [--fresh] [--config PATH] [--socket PATH] [--ax-bin PATH]

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedCommand {
    Help,
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

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
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
}
