//! Orchestrator naming, path, and cleanup helpers.
//!
//! This ports the low-risk helper portion of `internal/workspace/state.go`
//! and `internal/workspace/orchestrator.go`: stable orchestrator IDs,
//! artifact directory derivation, and cleanup of generated orchestrator
//! files/state.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ax_agent::{prepare_codex_home, remove_codex_home, Runtime, SUPPORTED_RUNTIMES};
use ax_config::ProjectNode;

use crate::{
    managed_run_agent_args, remove_mcp_config, write_mcp_config, write_orchestrator_prompt,
    TmuxBackend,
};

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("resolve home dir (HOME unset)")]
    HomeUnset,
    #[error("nil project node")]
    NilProjectNode,
    #[error("invalid orchestrator runtime for {name}: {runtime}")]
    InvalidRuntime { name: String, runtime: String },
    #[error("create orchestrator dir {path}: {source}")]
    CreateDir {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("create orchestrator claude dir {path}: {source}")]
    CreateClaudeDir {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    McpConfig(#[from] crate::McpConfigError),
    #[error(transparent)]
    Tmux(#[from] ax_tmux::TmuxError),
    #[error(transparent)]
    CodexHome(#[from] ax_agent::CodexHomeError),
    #[error(transparent)]
    WritePrompt(#[from] crate::OrchestratorPromptError),
    #[error("remove orchestrator instruction {path}: {source}")]
    RemoveInstruction {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("remove orchestrator .claude dir {path}: {source}")]
    RemoveClaudeDir {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("read orchestrator dir {path}: {source}")]
    ReadDir {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("remove empty orchestrator dir {path}: {source}")]
    RemoveDir {
        path: String,
        #[source]
        source: io::Error,
    },
}

/// The runtime identity of an orchestrator for a project prefix.
#[must_use]
pub fn orchestrator_name(prefix: &str) -> String {
    if prefix.is_empty() {
        "orchestrator".to_owned()
    } else {
        format!("{prefix}.orchestrator")
    }
}

/// `$HOME/.ax/orchestrator` for the root orchestrator.
pub fn root_orchestrator_dir() -> Result<PathBuf, OrchestratorError> {
    let home = std::env::var_os("HOME").ok_or(OrchestratorError::HomeUnset)?;
    Ok(PathBuf::from(home).join(".ax").join("orchestrator"))
}

/// Artifact directory for a project node's orchestrator.
pub fn orchestrator_dir_for_node(node: &ProjectNode) -> Result<PathBuf, OrchestratorError> {
    if node.prefix.is_empty() {
        return root_orchestrator_dir();
    }
    if node.dir.as_os_str().is_empty() {
        return Err(OrchestratorError::NilProjectNode);
    }
    let safe = node.prefix.replace('.', "_");
    Ok(node.dir.join(".ax").join(format!("orchestrator-{safe}")))
}

/// Ensure the generated orchestrator artifacts exist and optionally start
/// a managed session for sub-orchestrators.
pub fn ensure_orchestrator<B: TmuxBackend>(
    tmux: &B,
    node: &ProjectNode,
    parent_name: &str,
    socket_path: &Path,
    config_path: Option<&Path>,
    ax_bin: &Path,
    start_session: bool,
) -> Result<(), OrchestratorError> {
    let self_name = orchestrator_name(&node.prefix);
    let is_root = node.prefix.is_empty();
    let orch_dir = orchestrator_dir_for_node(node)?;
    let runtime = Runtime::normalize(&node.orchestrator_runtime).ok_or_else(|| {
        OrchestratorError::InvalidRuntime {
            name: self_name.clone(),
            runtime: node.orchestrator_runtime.clone(),
        }
    })?;

    fs::create_dir_all(&orch_dir).map_err(|source| OrchestratorError::CreateDir {
        path: orch_dir.display().to_string(),
        source,
    })?;
    let claude_dir = orch_dir.join(".claude");
    fs::create_dir_all(&claude_dir).map_err(|source| OrchestratorError::CreateClaudeDir {
        path: claude_dir.display().to_string(),
        source,
    })?;

    write_mcp_config(&orch_dir, &self_name, socket_path, config_path, ax_bin)?;
    if runtime == Runtime::Codex {
        prepare_codex_home(
            &self_name,
            &orch_dir.display().to_string(),
            socket_path,
            ax_bin,
            config_path,
        )?;
    }
    write_orchestrator_prompt(
        &orch_dir,
        node,
        &node.prefix,
        parent_name,
        runtime.as_str(),
        socket_path,
    )?;

    if !is_root && start_session && !tmux.session_exists(&self_name) {
        let argv =
            managed_run_agent_args(ax_bin, runtime, &self_name, socket_path, config_path, false);
        tmux.create_session_with_args(
            &self_name,
            &orch_dir.display().to_string(),
            &argv,
            &BTreeMap::new(),
        )?;
    }

    Ok(())
}

/// Remove generated orchestrator artifacts but leave unrelated files intact.
pub fn cleanup_orchestrator_artifacts(orch_dir: &Path) -> Result<(), OrchestratorError> {
    if orch_dir.as_os_str().is_empty() {
        return Ok(());
    }

    remove_mcp_config(orch_dir)?;

    for runtime in SUPPORTED_RUNTIMES {
        let path = orch_dir.join(runtime.instruction_file());
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(OrchestratorError::RemoveInstruction {
                    path: path.display().to_string(),
                    source,
                });
            }
        }
    }

    let claude_dir = orch_dir.join(".claude");
    match fs::remove_dir_all(&claude_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(OrchestratorError::RemoveClaudeDir {
                path: claude_dir.display().to_string(),
                source,
            });
        }
    }

    let entries = match fs::read_dir(orch_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(OrchestratorError::ReadDir {
                path: orch_dir.display().to_string(),
                source,
            });
        }
    };
    if entries.count() == 0 {
        match fs::remove_dir(orch_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(OrchestratorError::RemoveDir {
                    path: orch_dir.display().to_string(),
                    source,
                });
            }
        }
    }
    Ok(())
}

/// Remove any running orchestrator session plus generated artifacts/state.
pub fn cleanup_orchestrator_state<B: TmuxBackend>(
    tmux: &B,
    name: &str,
    orch_dir: &Path,
) -> Result<(), OrchestratorError> {
    if tmux.session_exists(name) {
        tmux.destroy_session(name)?;
    }
    cleanup_orchestrator_artifacts(orch_dir)?;
    remove_codex_home(name, &orch_dir.display().to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::orchestrator_name;

    #[test]
    fn orchestrator_name_uses_root_or_prefixed_identity() {
        assert_eq!(orchestrator_name(""), "orchestrator");
        assert_eq!(orchestrator_name("team"), "team.orchestrator");
        assert_eq!(orchestrator_name("team.sub"), "team.sub.orchestrator");
    }
}
