use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use crate::{claude_project_path, prepare_codex_home_for_launch, Runtime};

const CLAUDE_PROMPT_SUGGESTION_ENV: &str = "CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION";
const CLAUDE_PROMPT_SUGGESTION_DISABLED: &str = "false";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOptions {
    pub extra_args: Vec<String>,
    pub fresh_start: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("unsupported runtime {0:?}")]
    UnsupportedRuntime(String),
    #[error("resolve cwd: {0}")]
    ResolveCwd(#[source] io::Error),
    #[error("read instructions {path}: {source}")]
    ReadInstructions {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("remove claude project state {path}: {source}")]
    RemoveClaudeState {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    ClaudeProject(#[from] crate::ClaudeProjectError),
    #[error(transparent)]
    CodexHome(#[from] crate::CodexHomeError),
    #[error("launch {program}: {source}")]
    Launch {
        program: &'static str,
        #[source]
        source: io::Error,
    },
}

pub fn run_with_options(
    runtime_name: &str,
    workspace: &str,
    socket_path: &Path,
    ax_bin: &Path,
    config_path: Option<&Path>,
    options: &LaunchOptions,
) -> Result<ExitStatus, LaunchError> {
    let dir = std::env::current_dir().map_err(LaunchError::ResolveCwd)?;
    run_in_dir_with_options(
        runtime_name,
        &dir,
        workspace,
        socket_path,
        ax_bin,
        config_path,
        options,
    )
}

pub fn run_in_dir_with_options(
    runtime_name: &str,
    dir: &Path,
    workspace: &str,
    socket_path: &Path,
    ax_bin: &Path,
    config_path: Option<&Path>,
    options: &LaunchOptions,
) -> Result<ExitStatus, LaunchError> {
    let runtime = Runtime::normalize(runtime_name)
        .ok_or_else(|| LaunchError::UnsupportedRuntime(runtime_name.to_owned()))?;
    match runtime {
        Runtime::Claude => run_claude(dir, options),
        Runtime::Codex => run_codex(dir, workspace, socket_path, ax_bin, config_path, options),
    }
}

fn run_claude(dir: &Path, options: &LaunchOptions) -> Result<ExitStatus, LaunchError> {
    prepare_claude_launch(dir, options.fresh_start)?;

    if !options.extra_args.is_empty() {
        return spawn_claude(dir, false, &options.extra_args);
    }
    if options.fresh_start {
        return spawn_claude(dir, false, &[]);
    }

    let primary = spawn_claude(dir, true, &options.extra_args)?;
    if primary.success() {
        return Ok(primary);
    }
    spawn_claude(dir, false, &[])
}

fn spawn_claude(
    dir: &Path,
    continue_session: bool,
    extra_args: &[String],
) -> Result<ExitStatus, LaunchError> {
    let mut cmd = Command::new("claude");
    cmd.current_dir(dir)
        .args(claude_command_args(dir, continue_session, extra_args)?)
        .env(
            CLAUDE_PROMPT_SUGGESTION_ENV,
            CLAUDE_PROMPT_SUGGESTION_DISABLED,
        )
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd.status().map_err(|source| LaunchError::Launch {
        program: "claude",
        source,
    })
}

fn run_codex(
    dir: &Path,
    workspace: &str,
    socket_path: &Path,
    ax_bin: &Path,
    config_path: Option<&Path>,
    options: &LaunchOptions,
) -> Result<ExitStatus, LaunchError> {
    let codex_home = prepare_codex_home_for_launch(
        workspace,
        &dir.display().to_string(),
        socket_path,
        ax_bin,
        config_path,
        options.fresh_start,
    )?;

    let mut cmd = Command::new("codex");
    cmd.args(codex_command_args(dir, &options.extra_args))
        .env("CODEX_HOME", codex_home)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd.status().map_err(|source| LaunchError::Launch {
        program: "codex",
        source,
    })
}

fn claude_command_args(
    dir: &Path,
    continue_session: bool,
    extra_args: &[String],
) -> Result<Vec<String>, LaunchError> {
    let mut args = vec!["--dangerously-skip-permissions".to_owned()];
    if let Some(system_prompt) = load_instructions_file(dir, "CLAUDE.md")? {
        args.push("--append-system-prompt".to_owned());
        args.push(system_prompt);
    }
    if continue_session {
        args.push("--continue".to_owned());
    }
    args.extend(extra_args.iter().cloned());
    Ok(args)
}

fn codex_command_args(dir: &Path, extra_args: &[String]) -> Vec<String> {
    let mut args = vec![
        "--dangerously-bypass-approvals-and-sandbox".to_owned(),
        "--no-alt-screen".to_owned(),
        "-C".to_owned(),
        dir.display().to_string(),
    ];
    args.extend(extra_args.iter().cloned());
    args
}

fn load_instructions_file(dir: &Path, name: &str) -> Result<Option<String>, LaunchError> {
    let path = dir.join(name);
    match fs::read_to_string(&path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(LaunchError::ReadInstructions {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn prepare_claude_launch(dir: &Path, fresh: bool) -> Result<(), LaunchError> {
    if !fresh {
        return Ok(());
    }
    let project_path = claude_project_path(dir)?;
    match fs::remove_dir_all(&project_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(LaunchError::RemoveClaudeState {
            path: project_path.display().to_string(),
            source,
        }),
    }
}
