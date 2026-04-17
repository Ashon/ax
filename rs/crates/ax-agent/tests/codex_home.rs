use std::fs;
use std::path::Path;
use std::sync::Mutex;

use ax_agent::{prepare_codex_home, prepare_codex_home_for_launch};

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = HOME_LOCK.lock().unwrap();
    let prev = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("HOME", home);
    }
    let out = f();
    if let Some(value) = prev {
        unsafe {
            std::env::set_var("HOME", value);
        }
    } else {
        unsafe {
            std::env::remove_var("HOME");
        }
    }
    out
}

#[test]
fn prepare_codex_home_sets_shared_reasoning_effort_default() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        fs::create_dir_all(home.path().join(".codex")).unwrap();
        fs::write(
            home.path().join(".codex").join("config.toml"),
            "[projects.\"/tmp/existing\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();

        let codex_home = prepare_codex_home(
            "ws",
            "/tmp/workspace",
            Path::new("/tmp/ax.sock"),
            Path::new("/tmp/ax"),
            Some(&home.path().join("missing.yaml")),
        )
        .unwrap();

        let content = fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(content.contains("model_reasoning_effort = \"xhigh\""));
        assert!(content.contains("[mcp_servers.ax]"));
        assert!(content.contains("[projects.\"/tmp/workspace\"]"));
    });
}

#[test]
fn prepare_codex_home_uses_workspace_reasoning_override_from_ax_config() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        fs::create_dir_all(home.path().join(".codex")).unwrap();
        fs::write(
            home.path().join(".codex").join("config.toml"),
            "model_reasoning_effort = \"medium\"\n",
        )
        .unwrap();

        let config_path = home.path().join(".ax").join("config.yaml");
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            "\
codex_model_reasoning_effort: high
workspaces:
  ws:
    dir: .
    codex_model_reasoning_effort: low
",
        )
        .unwrap();

        let codex_home = prepare_codex_home(
            "ws",
            &home.path().display().to_string(),
            Path::new("/tmp/ax.sock"),
            Path::new("/tmp/ax"),
            Some(&config_path),
        )
        .unwrap();

        let content = fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(content.contains("model_reasoning_effort = \"low\""));
        assert!(!content.contains("model_reasoning_effort = \"medium\""));
    });
}

#[test]
fn prepare_codex_home_for_launch_fresh_removes_stale_state() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let codex_home = prepare_codex_home(
            "ws",
            "/tmp/workspace",
            Path::new("/tmp/ax.sock"),
            Path::new("/tmp/ax"),
            None,
        )
        .unwrap();

        let stale = codex_home.join("sessions").join("stale.json");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, "stale").unwrap();

        let refreshed = prepare_codex_home_for_launch(
            "ws",
            "/tmp/workspace",
            Path::new("/tmp/ax.sock"),
            Path::new("/tmp/ax"),
            None,
            true,
        )
        .unwrap();
        assert_eq!(refreshed, codex_home);
        assert!(!stale.exists());
        assert!(refreshed.join("config.toml").exists());
    });
}
