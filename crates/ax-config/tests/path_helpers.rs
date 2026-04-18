//! Sanity coverage for path helpers. Assertions are driven from
//! behaviour, not specific paths, so tests work on any test sandbox.

use std::fs;

use ax_config::{
    config_path_in_dir, default_config_path, find_config_file, legacy_config_path, ConfigRoot,
    DEFAULT_CONFIG_DIR, DEFAULT_CONFIG_FILE, LEGACY_CONFIG_FILE,
};

#[test]
fn default_config_path_matches_convention() {
    let got = default_config_path("/tmp/proj");
    assert_eq!(
        got,
        std::path::PathBuf::from(format!(
            "/tmp/proj/{DEFAULT_CONFIG_DIR}/{DEFAULT_CONFIG_FILE}"
        ))
    );
}

#[test]
fn legacy_config_path_is_flat() {
    let got = legacy_config_path("/tmp/proj");
    assert_eq!(
        got,
        std::path::PathBuf::from(format!("/tmp/proj/{LEGACY_CONFIG_FILE}"))
    );
}

#[test]
fn config_path_in_dir_prefers_dot_ax_over_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let preferred = default_config_path(root);
    let legacy = legacy_config_path(root);
    fs::create_dir_all(preferred.parent().unwrap()).unwrap();
    fs::write(&preferred, "project: demo\n").unwrap();
    fs::write(&legacy, "project: legacy\n").unwrap();

    let resolved = config_path_in_dir(root).expect("some");
    assert_eq!(resolved, preferred);
}

#[test]
fn config_path_in_dir_falls_back_to_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let legacy = legacy_config_path(root);
    fs::write(&legacy, "project: legacy\n").unwrap();
    let resolved = config_path_in_dir(root).expect("some");
    assert_eq!(resolved, legacy);
}

#[test]
fn find_config_file_walks_upward_and_returns_topmost() {
    // Point HOME at an empty sandbox so any config living in the real
    // $HOME/.ax/ (the developer's own ax setup) can't outrank the
    // fixture planted under tempdir.
    let home_sandbox = tempfile::tempdir().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let inner = root.join("a/b/c");
    fs::create_dir_all(&inner).unwrap();
    // Drop a config near the root and near the middle; `find` must pick
    // the topmost.
    let top_cfg = default_config_path(root);
    fs::create_dir_all(top_cfg.parent().unwrap()).unwrap();
    fs::write(&top_cfg, "project: top\n").unwrap();
    let mid_cfg = default_config_path(root.join("a"));
    fs::create_dir_all(mid_cfg.parent().unwrap()).unwrap();
    fs::write(&mid_cfg, "project: middle\n").unwrap();

    // SAFETY: This runs single-threaded inside the test, and find_config_file
    // reads $HOME synchronously without caching. std::env::set_var is
    // explicitly allowed in tests where we control concurrency.
    let prev_home = std::env::var_os("HOME");
    // SAFETY: set_var is unsafe on the ignored-return path; test runs
    // one thread at a time inside a #[test], so mutation is sound.
    unsafe {
        std::env::set_var("HOME", home_sandbox.path());
    }
    let resolved = find_config_file(&inner);
    if let Some(v) = prev_home {
        unsafe {
            std::env::set_var("HOME", v);
        }
    } else {
        unsafe {
            std::env::remove_var("HOME");
        }
    }

    assert_eq!(resolved.expect("some"), top_cfg);
}

#[test]
fn config_root_strips_dot_ax_component() {
    let root = ConfigRoot::from_config_path("/tmp/proj/.ax/config.yaml");
    assert_eq!(root.0, std::path::PathBuf::from("/tmp/proj"));
}

#[test]
fn config_root_keeps_legacy_parent() {
    let root = ConfigRoot::from_config_path("/tmp/proj/ax.yaml");
    assert_eq!(root.0, std::path::PathBuf::from("/tmp/proj"));
}
