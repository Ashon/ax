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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn claude_command_args_skips_prompt_injection_when_no_instructions_file() {
        let tmp = TempDir::new().expect("tempdir");
        let args = claude_command_args(tmp.path(), false, &[]).expect("build args");
        assert_eq!(args, vec!["--dangerously-skip-permissions".to_owned()]);
    }

    #[test]
    fn claude_command_args_injects_system_prompt_when_file_present() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(tmp.path().join("CLAUDE.md"), "act as an orchestrator").unwrap();
        let args = claude_command_args(tmp.path(), false, &[]).expect("build args");
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "--dangerously-skip-permissions");
        assert_eq!(args[1], "--append-system-prompt");
        assert_eq!(args[2], "act as an orchestrator");
    }

    #[test]
    fn claude_command_args_appends_continue_flag_when_requested() {
        let tmp = TempDir::new().expect("tempdir");
        let args = claude_command_args(tmp.path(), true, &[]).expect("build args");
        assert!(args.contains(&"--continue".to_owned()));
    }

    #[test]
    fn claude_command_args_appends_caller_extra_args_last() {
        let tmp = TempDir::new().expect("tempdir");
        let extras = vec!["--resume".to_owned(), "abc".to_owned()];
        let args = claude_command_args(tmp.path(), false, &extras).expect("build args");
        assert_eq!(&args[args.len() - 2..], extras.as_slice());
    }

    #[test]
    fn codex_command_args_always_include_base_flags_and_dir() {
        let tmp = TempDir::new().expect("tempdir");
        let args = codex_command_args(tmp.path(), &[]);
        assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_owned()));
        assert!(args.contains(&"--no-alt-screen".to_owned()));
        assert_eq!(args.iter().position(|a| a == "-C").map(|i| i + 1), Some(3));
        assert_eq!(args[3], tmp.path().display().to_string());
    }

    #[test]
    fn codex_command_args_appends_caller_extra_args_last() {
        let tmp = TempDir::new().expect("tempdir");
        let extras = vec!["--resume".to_owned(), "last".to_owned()];
        let args = codex_command_args(tmp.path(), &extras);
        assert_eq!(&args[args.len() - 2..], extras.as_slice());
    }

    #[test]
    fn load_instructions_file_returns_none_when_missing() {
        let tmp = TempDir::new().expect("tempdir");
        let result = load_instructions_file(tmp.path(), "CLAUDE.md").expect("load");
        assert_eq!(result, None);
    }

    #[test]
    fn load_instructions_file_returns_content_when_present() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(tmp.path().join("AGENTS.md"), "be helpful").unwrap();
        let result = load_instructions_file(tmp.path(), "AGENTS.md").expect("load");
        assert_eq!(result.as_deref(), Some("be helpful"));
    }

    #[test]
    fn run_in_dir_with_options_rejects_unknown_runtime() {
        let tmp = TempDir::new().expect("tempdir");
        let err = run_in_dir_with_options(
            "wolfram",
            tmp.path(),
            "orch",
            Path::new("/tmp/ax.sock"),
            Path::new("/tmp/ax"),
            None,
            &LaunchOptions::default(),
        )
        .expect_err("unknown runtime must be rejected before launch");
        match err {
            LaunchError::UnsupportedRuntime(name) => assert_eq!(name, "wolfram"),
            other => panic!("expected UnsupportedRuntime, got {other:?}"),
        }
    }
}
