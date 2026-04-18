//! Verify the schema parses real-world `.ax/config.yaml` fixtures and
//! that the materialised values match the source-of-truth semantics.
//!
//! Exact byte-level YAML round-tripping is not something we pursue;
//! YAML writers differ on indentation, quoting, and key ordering. What
//! matters is that we parse the same content to the same in-memory
//! model.

use ax_config::{Child, Config, Workspace};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn load(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/{name}"))
        .unwrap_or_else(|e| panic!("read {name}: {e}"))
}

#[test]
fn parses_go_produced_config_yaml() {
    let raw = load("sample_config.yaml");
    let cfg = Config::from_yaml(&raw).expect("parse");

    assert_eq!(cfg.project, "demo");
    assert_eq!(cfg.orchestrator_runtime, "codex");
    assert_eq!(cfg.codex_model_reasoning_effort, "xhigh");
    assert_eq!(cfg.idle_timeout_minutes, 30);
    assert_eq!(cfg.idle_timeout_minutes_or_default(), 30);
    assert!(!cfg.disable_root_orchestrator);
    assert!(!cfg.experimental_mcp_team_reconfigure);

    let main = cfg.workspaces.get("main").expect("main workspace");
    assert_eq!(main.dir, ".");
    assert_eq!(main.description, "Main workspace");
    assert_eq!(main.runtime, "codex");

    let worker = cfg.workspaces.get("worker").expect("worker workspace");
    assert_eq!(worker.dir, "./worker");
    assert_eq!(worker.env.get("FOO").unwrap(), "bar");
    assert_eq!(worker.env.get("BAZ").unwrap(), "qux");

    let child = cfg.children.get("sub").expect("sub child");
    assert_eq!(child.dir, "./subproject");
    assert_eq!(child.prefix, "sub");
}

#[test]
fn default_for_runtime_matches_go_defaults() {
    let cfg = Config::default_for_runtime("demo", "claude");
    assert_eq!(cfg.project, "demo");
    assert_eq!(cfg.orchestrator_runtime, "claude");
    assert_eq!(cfg.codex_model_reasoning_effort, "xhigh");
    assert_eq!(cfg.idle_timeout_minutes, 15);
    let main = cfg.workspaces.get("main").expect("main workspace");
    assert_eq!(main.dir, ".");
    assert_eq!(main.runtime, "claude");
}

#[test]
fn idle_timeout_defaults_when_zero() {
    let mut cfg = Config::default_for_runtime("demo", "claude");
    cfg.idle_timeout_minutes = 0;
    assert_eq!(cfg.idle_timeout_minutes_or_default(), 15);
    cfg.idle_timeout_minutes = 45;
    assert_eq!(cfg.idle_timeout_minutes_or_default(), 45);
}

#[test]
fn save_then_read_local_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");

    let original = {
        let mut cfg = Config::default_for_runtime("demo", "codex");
        cfg.workspaces.insert(
            "worker".to_owned(),
            Workspace {
                dir: "./worker".to_owned(),
                description: "Worker agent".to_owned(),
                runtime: "codex".to_owned(),
                ..Default::default()
            },
        );
        cfg.children.insert(
            "sub".to_owned(),
            Child {
                dir: "./subproject".to_owned(),
                prefix: "sub".to_owned(),
            },
        );
        cfg
    };

    original.save(&path).expect("save");
    let reloaded = Config::read_local(&path).expect("read");
    assert_eq!(reloaded.project, "demo");
    assert_eq!(reloaded.workspaces.len(), 2);
    assert!(reloaded.children.contains_key("sub"));
}
