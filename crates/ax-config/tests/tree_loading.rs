//! Recursive child loading and tree construction tests.

use std::fs;
use std::path::Path;

use ax_config::{default_config_path, Config};

fn write_config(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn load_merges_child_workspaces_with_prefix() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let child = root.join("child");

    write_config(
        &default_config_path(root),
        "project: parent\nchildren:\n  sub:\n    dir: ./child\n    prefix: team\nworkspaces:\n  main:\n    dir: .\n    runtime: codex\n",
    );
    write_config(
        &default_config_path(&child),
        "project: child\nworkspaces:\n  worker:\n    dir: .\n    runtime: codex\n",
    );

    let cfg = Config::load(default_config_path(root)).expect("load");
    assert!(cfg.workspaces.contains_key("main"), "main present");
    assert!(
        cfg.workspaces.contains_key("team.worker"),
        "child workspace merged under prefix: {:?}",
        cfg.workspaces.keys().collect::<Vec<_>>()
    );
}

#[test]
fn load_merges_child_agent_providers_and_applies_child_default() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let child = root.join("child");

    write_config(
        &default_config_path(root),
        "project: parent\nchildren:\n  sub:\n    dir: ./child\n    prefix: team\n",
    );
    write_config(
        &default_config_path(&child),
        "\
project: child
default_agent_provider: local
agent_providers:
  local:
    runtime: codex
    base_url: http://127.0.0.1:8000/v1
workspaces:
  worker:
    dir: .
    runtime: codex
",
    );

    let cfg = Config::load(default_config_path(root)).expect("load");
    assert!(cfg.agent_providers.contains_key("local"));
    assert_eq!(
        cfg.workspaces
            .get("team.worker")
            .expect("child workspace")
            .agent_provider,
        "local"
    );
}

#[test]
fn load_tree_preserves_hierarchy() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let child = root.join("child");

    write_config(
        &default_config_path(root),
        "project: parent\nchildren:\n  sub:\n    dir: ./child\n    prefix: team\nworkspaces:\n  main:\n    dir: .\n    runtime: codex\n",
    );
    write_config(
        &default_config_path(&child),
        "project: child\nworkspaces:\n  worker:\n    dir: .\n    runtime: codex\n",
    );

    let tree = Config::load_tree(default_config_path(root)).expect("tree");
    assert_eq!(tree.name, "parent");
    assert_eq!(tree.prefix, "");
    assert_eq!(tree.workspaces.len(), 1);
    assert_eq!(tree.workspaces[0].merged_name, "main");

    assert_eq!(tree.children.len(), 1);
    let child_node = &tree.children[0];
    assert_eq!(child_node.alias, "sub");
    assert_eq!(child_node.prefix, "team");
    assert_eq!(child_node.name, "child");
    assert_eq!(child_node.workspaces.len(), 1);
    assert_eq!(child_node.workspaces[0].merged_name, "team.worker");
}

#[test]
fn load_skips_stale_child_without_config() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();

    write_config(
        &default_config_path(root),
        "project: parent\nchildren:\n  ghost:\n    dir: ./missing\n    prefix: team\nworkspaces:\n  main:\n    dir: .\n    runtime: codex\n",
    );

    let cfg = Config::load(default_config_path(root)).expect("should tolerate missing child");
    assert!(cfg.workspaces.contains_key("main"));
    assert_eq!(
        cfg.workspaces
            .keys()
            .filter(|k| k.starts_with("team."))
            .count(),
        0
    );
}

#[test]
fn default_child_prefix_falls_back_to_alias() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let child = root.join("child");

    // No `prefix:` on the child -- normalize should treat the alias
    // ("kid") as the implicit prefix, matching normalizeLocalConfig.
    write_config(
        &default_config_path(root),
        "project: parent\nchildren:\n  kid:\n    dir: ./child\nworkspaces:\n  main:\n    dir: .\n    runtime: codex\n",
    );
    write_config(
        &default_config_path(&child),
        "project: child\nworkspaces:\n  worker:\n    dir: .\n    runtime: codex\n",
    );

    let cfg = Config::load(default_config_path(root)).expect("load");
    assert!(
        cfg.workspaces.contains_key("kid.worker"),
        "expected child workspace to inherit alias as prefix: {:?}",
        cfg.workspaces.keys().collect::<Vec<_>>()
    );
}
