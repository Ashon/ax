//! Helpers for the Claude Code per-project state directory.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ClaudeProjectError {
    #[error("resolve home dir (HOME unset)")]
    HomeUnset,
}

/// Return the `~/.claude/projects/<encoded-cwd>` directory where Claude
/// Code stores project-specific session state. The encoding replaces
/// `/` and `.` with `-` in the absolute dir path.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_preserves_absolute_root() {
        assert_eq!(normalize_path(Path::new("/")), "/");
        assert_eq!(normalize_path(Path::new("/tmp/ws")), "/tmp/ws");
    }

    #[test]
    fn normalize_path_collapses_redundant_separators_and_dots() {
        assert_eq!(normalize_path(Path::new("/tmp//ws/./sub")), "/tmp/ws/sub");
        assert_eq!(normalize_path(Path::new("tmp//./ws")), "tmp/ws");
    }

    #[test]
    fn normalize_path_resolves_double_dot_within_absolute_path() {
        assert_eq!(normalize_path(Path::new("/a/b/../c")), "/a/c");
        assert_eq!(normalize_path(Path::new("/a/../../b")), "/b");
    }

    #[test]
    fn normalize_path_returns_dot_for_empty_relative_components() {
        assert_eq!(normalize_path(Path::new(".")), ".");
        assert_eq!(normalize_path(Path::new("./.")), ".");
    }

    #[test]
    fn encode_project_key_replaces_slashes_and_dots_with_dashes() {
        assert_eq!(encode_project_key("/tmp/my.project"), "-tmp-my-project");
        assert_eq!(encode_project_key("/a/b/c"), "-a-b-c");
        assert_eq!(encode_project_key("relative.dir"), "relative-dir");
    }

}
