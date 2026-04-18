//! Tilde expansion for the default socket path. `~` and `~/...` are
//! resolved against `$HOME`; anything else passes through.

use std::path::{Path, PathBuf};

pub const DEFAULT_SOCKET_PATH: &str = "~/.local/state/ax/daemon.sock";

#[must_use]
pub fn expand_socket_path(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).and_then(|p| {
        if Path::new(&p).as_os_str().is_empty() {
            None
        } else {
            Some(p)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_tilde_resolves_to_home() {
        std::env::set_var("HOME", "/tmp/fake-home");
        assert_eq!(expand_socket_path("~"), PathBuf::from("/tmp/fake-home"));
    }

    #[test]
    fn tilde_prefix_joins_home() {
        std::env::set_var("HOME", "/tmp/fake-home");
        assert_eq!(
            expand_socket_path("~/.local/state/ax/daemon.sock"),
            PathBuf::from("/tmp/fake-home/.local/state/ax/daemon.sock"),
        );
    }

    #[test]
    fn absolute_path_passes_through() {
        assert_eq!(
            expand_socket_path("/var/run/ax.sock"),
            PathBuf::from("/var/run/ax.sock"),
        );
    }
}
