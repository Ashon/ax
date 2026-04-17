//! Workspace artifact + tmux lifecycle management.
//!
//! Mirrors the `EnsureArtifacts`, `CleanupWorkspaceState`, and
//! `Manager::{Create,Restart,Destroy}` pieces of
//! `internal/workspace/workspace.go`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ax_agent::{prepare_codex_home, remove_codex_home, Runtime};
use ax_config::Workspace;
use ax_tmux::{
    create_session, create_session_with_args, create_session_with_command, destroy_session,
};

use crate::{
    remove_instructions, remove_mcp_config, write_instructions, write_mcp_config,
    InstructionsError, McpConfigError,
};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("unsupported runtime {0:?}")]
    UnsupportedRuntime(String),
    #[error("create workspace dir {path}: {source}")]
    CreateDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("tmux session {0:?} already exists")]
    SessionExists(String),
    #[error(transparent)]
    Tmux(#[from] ax_tmux::TmuxError),
    #[error(transparent)]
    McpConfig(#[from] McpConfigError),
    #[error(transparent)]
    Instructions(#[from] InstructionsError),
    #[error(transparent)]
    CodexHome(#[from] ax_agent::CodexHomeError),
}

pub trait TmuxBackend {
    fn session_exists(&self, workspace: &str) -> bool;
    fn create_session(
        &self,
        workspace: &str,
        dir: &str,
        shell: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError>;
    fn create_session_with_command(
        &self,
        workspace: &str,
        dir: &str,
        command: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError>;
    fn create_session_with_args(
        &self,
        workspace: &str,
        dir: &str,
        argv: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError>;
    fn destroy_session(&self, workspace: &str) -> Result<(), ax_tmux::TmuxError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RealTmux;

impl TmuxBackend for RealTmux {
    fn session_exists(&self, workspace: &str) -> bool {
        ax_tmux::session_exists(workspace)
    }

    fn create_session(
        &self,
        workspace: &str,
        dir: &str,
        shell: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        create_session(
            workspace,
            dir,
            shell,
            &ax_tmux::CreateOptions { env: env.clone() },
        )
    }

    fn create_session_with_command(
        &self,
        workspace: &str,
        dir: &str,
        command: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        create_session_with_command(
            workspace,
            dir,
            command,
            &ax_tmux::CreateOptions { env: env.clone() },
        )
    }

    fn create_session_with_args(
        &self,
        workspace: &str,
        dir: &str,
        argv: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        create_session_with_args(
            workspace,
            dir,
            &refs,
            &ax_tmux::CreateOptions { env: env.clone() },
        )
    }

    fn destroy_session(&self, workspace: &str) -> Result<(), ax_tmux::TmuxError> {
        destroy_session(workspace)
    }
}

pub struct Manager<B = RealTmux> {
    socket_path: PathBuf,
    config_path: Option<PathBuf>,
    ax_bin: PathBuf,
    tmux: B,
}

impl Manager<RealTmux> {
    #[must_use]
    pub fn new(
        socket_path: impl Into<PathBuf>,
        config_path: Option<PathBuf>,
        ax_bin: impl Into<PathBuf>,
    ) -> Self {
        Self {
            socket_path: socket_path.into(),
            config_path,
            ax_bin: ax_bin.into(),
            tmux: RealTmux,
        }
    }
}

impl<B: TmuxBackend> Manager<B> {
    #[must_use]
    pub fn with_tmux(
        socket_path: impl Into<PathBuf>,
        config_path: Option<PathBuf>,
        ax_bin: impl Into<PathBuf>,
        tmux: B,
    ) -> Self {
        Self {
            socket_path: socket_path.into(),
            config_path,
            ax_bin: ax_bin.into(),
            tmux,
        }
    }

    pub fn create(&self, name: &str, workspace: &Workspace) -> Result<(), WorkspaceError> {
        self.create_inner(name, workspace, false)
    }

    pub fn restart(&self, name: &str, workspace: &Workspace) -> Result<(), WorkspaceError> {
        cleanup_workspace_state(&self.tmux, name, &workspace.dir)?;
        self.create_inner(name, workspace, true)
    }

    pub fn destroy(&self, name: &str, dir: &str) -> Result<(), WorkspaceError> {
        if self.tmux.session_exists(name) {
            self.tmux.destroy_session(name)?;
        }
        if dir.trim().is_empty() {
            return Ok(());
        }
        let dir_path = Path::new(dir);
        remove_mcp_config(dir_path)?;
        remove_instructions(dir_path)?;
        Ok(())
    }

    fn create_inner(
        &self,
        name: &str,
        workspace: &Workspace,
        fresh: bool,
    ) -> Result<(), WorkspaceError> {
        let runtime = runtime_for_name(&workspace.runtime)?;
        ensure_artifacts(
            name,
            workspace,
            &self.socket_path,
            self.config_path.as_deref(),
            &self.ax_bin,
        )?;

        if self.tmux.session_exists(name) {
            return Err(WorkspaceError::SessionExists(ax_tmux::session_name(name)));
        }

        if !workspace.agent.is_empty() {
            if workspace.agent == "none" {
                self.tmux
                    .create_session(name, &workspace.dir, &workspace.shell, &workspace.env)?;
                return Ok(());
            }
            self.tmux.create_session_with_command(
                name,
                &workspace.dir,
                &workspace.agent,
                &workspace.env,
            )?;
            return Ok(());
        }

        let argv = managed_run_agent_args(
            &self.ax_bin,
            runtime,
            name,
            &self.socket_path,
            self.config_path.as_deref(),
            fresh,
        );
        self.tmux
            .create_session_with_args(name, &workspace.dir, &argv, &workspace.env)?;
        Ok(())
    }
}

pub fn ensure_artifacts(
    name: &str,
    workspace: &Workspace,
    socket_path: &Path,
    config_path: Option<&Path>,
    ax_bin: &Path,
) -> Result<(), WorkspaceError> {
    let runtime = runtime_for_name(&workspace.runtime)?;
    let dir = Path::new(&workspace.dir);
    fs::create_dir_all(dir).map_err(|source| WorkspaceError::CreateDir {
        path: workspace.dir.clone(),
        source,
    })?;
    write_mcp_config(dir, name, socket_path, config_path, ax_bin)?;
    write_instructions(dir, name, runtime.as_str(), &workspace.instructions)?;
    if runtime == Runtime::Codex {
        prepare_codex_home(name, &workspace.dir, socket_path, ax_bin, config_path)?;
    }
    Ok(())
}

pub fn cleanup_workspace_artifacts(name: &str, dir: &str) -> Result<(), WorkspaceError> {
    if dir.trim().is_empty() {
        return Ok(());
    }
    let dir_path = Path::new(dir);
    remove_mcp_config(dir_path)?;
    remove_instructions(dir_path)?;
    remove_codex_home(name, dir)?;
    Ok(())
}

pub fn managed_run_agent_args(
    ax_bin: &Path,
    runtime: Runtime,
    workspace: &str,
    socket_path: &Path,
    config_path: Option<&Path>,
    fresh: bool,
) -> Vec<String> {
    let mut args = vec![
        ax_bin.display().to_string(),
        "run-agent".to_owned(),
        "--runtime".to_owned(),
        runtime.as_str().to_owned(),
        "--workspace".to_owned(),
        workspace.to_owned(),
        "--socket".to_owned(),
        socket_path.display().to_string(),
    ];
    if let Some(path) = config_path {
        args.push("--config".to_owned());
        args.push(path.display().to_string());
    }
    if fresh {
        args.push("--fresh".to_owned());
    }
    args
}

fn cleanup_workspace_state<B: TmuxBackend>(
    tmux: &B,
    name: &str,
    dir: &str,
) -> Result<(), WorkspaceError> {
    if tmux.session_exists(name) {
        tmux.destroy_session(name)?;
    }
    cleanup_workspace_artifacts(name, dir)
}

fn runtime_for_name(name: &str) -> Result<Runtime, WorkspaceError> {
    Runtime::normalize(name).ok_or_else(|| WorkspaceError::UnsupportedRuntime(name.to_owned()))
}
