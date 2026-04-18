//! Helpers for the Claude Code per-project state directory.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ClaudeProjectError {
    #[error("resolve home dir (HOME unset)")]
    HomeUnset,
}

/// Return the `~/.claude/projects/<encoded-cwd>` directory where Claude
/// Code stores project-specific session state. The encoding mirrors the
/// one Go uses: replace `/` and `.` with `-` in the absolute dir path.
pub fn claude_project_path(dir: &Path) -> Result<PathBuf, ClaudeProjectError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(ClaudeProjectError::HomeUnset)?;
    let cleaned = normalize_path(dir);
    let encoded = encode_project_key(&cleaned);
    Ok(home.join(".claude").join("projects").join(encoded))
}

fn encode_project_key(dir: &str) -> String {
    dir.replace(['/', '.'], "-")
}

/// Simple path cleaning matching `filepath.Clean` for the inputs the ax
/// agent launcher produces: preserve absolute leading `/`, collapse
/// redundant separators and `.` components. This is not a full
/// path-normalizer; it's only used for the Claude project key.
fn normalize_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let trimmed = raw.trim();
    let absolute = trimmed.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in trimmed.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() && !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return if absolute { "/".into() } else { ".".into() };
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}
