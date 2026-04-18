//! `.mcp.json` management.
//!
//! The daemon registers itself as the "ax" MCP server inside the
//! workspace's local `.mcp.json` so the runtime (Claude or Codex)
//! launches it on every turn. We merge into any pre-existing file
//! instead of overwriting so users can keep unrelated MCP servers in
//! the same file.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MCP_CONFIG_FILE: &str = ".mcp.json";
const AX_SERVER_KEY: &str = "ax";

#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    #[error("read {path:?}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("write {path:?}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("encode mcp config: {0}")]
    Encode(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct McpConfig {
    #[serde(rename = "mcpServers")]
    mcp_servers: BTreeMap<String, McpServerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct McpServerEntry {
    command: String,
    args: Vec<String>,
}

/// Write (or merge into) `<dir>/.mcp.json` with an "ax" MCP server
/// pointing at `ax_bin mcp-server --workspace <workspace> --socket
/// <socket_path>` (plus `--config <config_path>` when non-empty).
pub fn write_mcp_config(
    dir: &Path,
    workspace: &str,
    socket_path: &Path,
    config_path: Option<&Path>,
    ax_bin: &Path,
) -> Result<(), McpConfigError> {
    let mut args = vec![
        "mcp-server".to_owned(),
        "--workspace".to_owned(),
        workspace.to_owned(),
        "--socket".to_owned(),
        socket_path.display().to_string(),
    ];
    if let Some(cfg) = config_path {
        let cfg_str = cfg.display().to_string();
        if !cfg_str.is_empty() {
            args.push("--config".to_owned());
            args.push(cfg_str);
        }
    }
    let ax_entry = McpServerEntry {
        command: ax_bin.display().to_string(),
        args,
    };

    let path = dir.join(MCP_CONFIG_FILE);
    let mut cfg = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<McpConfig>(&bytes).unwrap_or_else(|_| empty()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => empty(),
        Err(source) => return Err(McpConfigError::Read { path, source }),
    };
    cfg.mcp_servers.insert(AX_SERVER_KEY.to_owned(), ax_entry);

    let mut body = serde_json::to_vec_pretty(&cfg)?;
    body.push(b'\n');
    fs::write(&path, body).map_err(|source| McpConfigError::Write { path, source })
}

/// Drop the "ax" entry from `<dir>/.mcp.json`. Deletes the file when
/// it would otherwise become empty.
pub fn remove_mcp_config(dir: &Path) -> Result<(), McpConfigError> {
    let path = dir.join(MCP_CONFIG_FILE);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(McpConfigError::Read { path, source }),
    };
    let Ok(mut cfg) = serde_json::from_slice::<McpConfig>(&bytes) else {
        return Ok(());
    };
    cfg.mcp_servers.remove(AX_SERVER_KEY);
    if cfg.mcp_servers.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(McpConfigError::Write { path, source }),
        }
    }
    let mut body = serde_json::to_vec_pretty(&cfg)?;
    body.push(b'\n');
    fs::write(&path, body).map_err(|source| McpConfigError::Write { path, source })
}

fn empty() -> McpConfig {
    McpConfig {
        mcp_servers: BTreeMap::new(),
    }
}
