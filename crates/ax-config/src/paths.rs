//! Path helpers for locating `.ax/config.yaml`. Mirrors the path-related
//! functions in `internal/config/config.go`.

use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_DIR: &str = ".ax";
pub const DEFAULT_CONFIG_FILE: &str = "config.yaml";
pub const LEGACY_CONFIG_FILE: &str = "ax.yaml";

/// The root directory of an ax project (the parent of `.ax/` when the
/// config lives under `.ax/config.yaml`, or the directory containing
/// `ax.yaml` in the legacy layout).
#[derive(Debug, Clone)]
pub struct ConfigRoot(pub PathBuf);

/// Returns the conventional `<dir>/.ax/config.yaml` path without checking
/// whether it exists.
pub fn default_config_path(dir: impl AsRef<Path>) -> PathBuf {
    dir.as_ref()
        .join(DEFAULT_CONFIG_DIR)
        .join(DEFAULT_CONFIG_FILE)
}

/// Returns the legacy `<dir>/ax.yaml` path without checking whether it
/// exists.
pub fn legacy_config_path(dir: impl AsRef<Path>) -> PathBuf {
    dir.as_ref().join(LEGACY_CONFIG_FILE)
}

/// Resolve the ax config for a directory, preferring the newer
/// `.ax/config.yaml` layout and falling back to `ax.yaml`. Returns `None`
/// when neither file exists.
pub fn config_path_in_dir(dir: impl AsRef<Path>) -> Option<PathBuf> {
    let dir = dir.as_ref();
    let preferred = default_config_path(dir);
    if preferred.is_file() {
        return Some(preferred);
    }
    let legacy = legacy_config_path(dir);
    if legacy.is_file() {
        return Some(legacy);
    }
    None
}

/// Walk upward from `start` and return the *topmost* ancestor that
/// contains an ax config. Also checks `$HOME` so a global config wins.
/// Mirrors `config.FindConfigFile` in the Go implementation.
pub fn find_config_file(start: impl AsRef<Path>) -> Option<PathBuf> {
    let mut topmost: Option<PathBuf> = None;
    let mut current = start.as_ref().to_path_buf();
    loop {
        if let Some(path) = config_path_in_dir(&current) {
            topmost = Some(path);
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }
    if let Some(home) = dirs_home() {
        if let Some(path) = config_path_in_dir(&home) {
            topmost = Some(path);
        }
    }
    topmost
}

fn dirs_home() -> Option<PathBuf> {
    // Avoid an extra dependency: $HOME is how the Go code expresses this on
    // all the platforms we target today (macOS + Linux).
    std::env::var_os("HOME").map(PathBuf::from)
}

impl ConfigRoot {
    /// Given an absolute path to a config file, return the project root
    /// (the directory that owns `.ax/`).
    #[must_use]
    pub fn from_config_path(path: impl AsRef<Path>) -> Self {
        let parent = path.as_ref().parent().unwrap_or(Path::new("."));
        let base = parent
            .file_name()
            .and_then(|os| os.to_str())
            .unwrap_or_default();
        if base == DEFAULT_CONFIG_DIR {
            Self(parent.parent().unwrap_or(Path::new(".")).to_path_buf())
        } else {
            Self(parent.to_path_buf())
        }
    }
}
