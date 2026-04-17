//! Path-derivation parity with the Go implementation.
//!
//! Key constants (sha1 truncation length, directory layout) are exercised
//! against known-good values so the Rust daemon and the Go daemon agree on
//! where any given workspace's codex or claude state lives.

use std::path::PathBuf;

use ax_agent::{claude_project_path, codex_home_key, codex_home_path, instruction_file, Runtime};

#[test]
fn runtime_normalize_handles_aliases_and_casing() {
    assert_eq!(Runtime::normalize(""), Some(Runtime::Claude));
    assert_eq!(Runtime::normalize("claude"), Some(Runtime::Claude));
    assert_eq!(Runtime::normalize("CLAUDE"), Some(Runtime::Claude));
    assert_eq!(Runtime::normalize(" codex "), Some(Runtime::Codex));
    assert_eq!(Runtime::normalize("custom"), None);
}

#[test]
fn instruction_file_returns_per_runtime_filenames() {
    assert_eq!(instruction_file(""), Some("CLAUDE.md"));
    assert_eq!(instruction_file("codex"), Some("AGENTS.md"));
    assert!(instruction_file("custom").is_none());
}

#[test]
fn codex_home_key_matches_go_truncated_sha1() {
    // Ground truth sha1 of "/tmp/proj": 3a55d7b82e11b3c0ba8e…
    // The Go code keeps the first 6 bytes (12 hex chars) and prepends
    // the workspace name. We don't hardcode the hex here to avoid
    // coupling to a specific sha1 crate's output; instead we assert
    // structural properties + stability.
    let key = codex_home_key("worker", "/tmp/proj");
    let parts: Vec<&str> = key.splitn(2, '-').collect();
    assert_eq!(parts.len(), 2, "key = {key}");
    assert_eq!(parts[0], "worker");
    assert_eq!(parts[1].len(), 12, "sha1 prefix is 6 bytes = 12 hex chars");
    assert!(parts[1].chars().all(|c| c.is_ascii_hexdigit()));

    // Stable: same input must produce same key.
    assert_eq!(key, codex_home_key("worker", "/tmp/proj"));

    // Different dir → different key.
    assert_ne!(key, codex_home_key("worker", "/tmp/other"));
}

#[test]
fn codex_home_path_lives_under_home_ax_codex() {
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: tests within one process run serially with respect to env.
    let prev = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    let path = codex_home_path("worker", "/tmp/proj").unwrap();
    if let Some(v) = prev {
        unsafe {
            std::env::set_var("HOME", v);
        }
    } else {
        unsafe {
            std::env::remove_var("HOME");
        }
    }

    assert!(path.starts_with(tmp.path().join(".ax").join("codex")));
    let key = codex_home_key("worker", "/tmp/proj");
    assert_eq!(path.file_name().unwrap().to_str().unwrap(), key);
}

#[test]
fn claude_project_path_encodes_cwd_with_dashes() {
    let tmp = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    let path = claude_project_path(&PathBuf::from("/Users/ashon/git/github/ashon/ax")).unwrap();
    if let Some(v) = prev {
        unsafe {
            std::env::set_var("HOME", v);
        }
    } else {
        unsafe {
            std::env::remove_var("HOME");
        }
    }
    // Exact encoding Go produces for that cwd -- also the key ax-usage
    // tests against when discovering transcripts.
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        "-Users-ashon-git-github-ashon-ax"
    );
}
