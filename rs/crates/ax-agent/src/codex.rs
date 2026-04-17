//! ax-managed `CODEX_HOME` directory helpers.
//!
//! Every workspace running codex gets an isolated config dir under
//! `~/.ax/codex/<workspace>-<sha1>`. The sha1 is truncated to 6 bytes
//! (12 hex chars) and derived from the workspace's base directory — the
//! same truncation Go's `codexHomeKey` uses so a Rust daemon and a Go
//! daemon resolve the same path.

use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

#[derive(Debug, thiserror::Error)]
pub enum CodexHomeError {
    #[error("resolve home dir (HOME unset)")]
    HomeUnset,
    #[error("remove codex home {path}: {source}")]
    Remove {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Stable per-workspace key used as the directory name. sha1(dir)[0..6]
/// hex-encoded, suffixed onto the workspace name.
#[must_use]
pub fn codex_home_key(workspace: &str, dir: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(dir.as_bytes());
    let digest = hasher.finalize();
    let truncated = &digest[..6];
    format!("{workspace}-{}", hex::encode(truncated))
}

/// Returns `$HOME/.ax/codex/<workspace>-<hash>` for the given workspace.
pub fn codex_home_path(workspace: &str, dir: &str) -> Result<PathBuf, CodexHomeError> {
    let home = resolve_home()?;
    Ok(home
        .join(".ax")
        .join("codex")
        .join(codex_home_key(workspace, dir)))
}

/// Delete the managed `CODEX_HOME` directory for a workspace. Silently
/// succeeds when the directory doesn't exist.
pub fn remove_codex_home(workspace: &str, dir: &str) -> Result<(), CodexHomeError> {
    let path = codex_home_path(workspace, dir)?;
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CodexHomeError::Remove {
            path: path.display().to_string(),
            source: e,
        }),
    }
}

fn resolve_home() -> Result<PathBuf, CodexHomeError> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(CodexHomeError::HomeUnset)
}

/// Exposed for integration tests elsewhere in the workspace (mostly
/// `ax-usage`): check whether a path looks like it sits under an
/// ax-managed codex home.
#[must_use]
pub fn is_managed_codex_home(path: &Path) -> bool {
    path.ancestors().any(|p| {
        p.file_name()
            .and_then(|os| os.to_str())
            .is_some_and(|s| s == "codex")
            && p.parent()
                .and_then(Path::file_name)
                .and_then(|os| os.to_str())
                == Some(".ax")
    })
}
