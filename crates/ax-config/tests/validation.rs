//! Structural validation coverage. Errors wrap into `TreeError::Validation`
//! and are surfaced through `Config::load`'s pre-pass.

use std::fs;
use std::path::Path;

use ax_config::{default_config_path, Config, TreeError, ValidationError};

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// Unwrap the boxed `ValidationError` out of a failed `Config::load`.
fn validation_err(err: TreeError) -> ValidationError {
    match err {
        TreeError::Validation(boxed) => *boxed,
        other => panic!("expected TreeError::Validation, got {other:?}"),
    }
}

#[test]
fn duplicate_workspace_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &default_config_path(root),
        "\
project: demo
workspaces:
  a:
    dir: ./shared
    runtime: claude
  b:
    dir: ./shared
    runtime: claude
",
    );
    let err = validation_err(Config::load(default_config_path(root)).unwrap_err());
    assert!(
        matches!(err, ValidationError::DuplicateWorkspaceDir { .. }),
        "got {err:?}"
    );
}

#[test]
fn reserved_orchestrator_name_blocks_workspace_named_orchestrator() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &default_config_path(root),
        "\
project: demo
workspaces:
  orchestrator:
    dir: ./orch
    runtime: claude
",
    );
    let err = validation_err(Config::load(default_config_path(root)).unwrap_err());
    match err {
        ValidationError::ReservedNameForWorkspace { merged, .. } => {
            assert_eq!(merged, "orchestrator");
        }
        other => panic!("expected ReservedNameForWorkspace, got {other:?}"),
    }
}

#[test]
fn disable_root_orchestrator_unblocks_the_workspace_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &default_config_path(root),
        "\
project: demo
disable_root_orchestrator: true
workspaces:
  orchestrator:
    dir: ./orch
    runtime: claude
",
    );
    let cfg = Config::load(default_config_path(root)).expect("load");
    assert!(cfg.workspaces.contains_key("orchestrator"));
}

#[test]
fn duplicate_child_prefix_between_subprojects_fails() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let a = root.join("a");
    let b = root.join("b");
    write(
        &default_config_path(root),
        "\
project: parent
children:
  alpha:
    dir: ./a
    prefix: team
  bravo:
    dir: ./b
    prefix: team
workspaces:
  main:
    dir: .
    runtime: claude
",
    );
    write(
        &default_config_path(&a),
        "project: a\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&b),
        "project: b\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    let err = validation_err(Config::load(default_config_path(root)).unwrap_err());
    match err {
        ValidationError::DuplicateChildPrefix { prefix, .. } => {
            assert_eq!(prefix, "team");
        }
        other => panic!("expected DuplicateChildPrefix, got {other:?}"),
    }
}

#[test]
fn child_prefix_colliding_with_workspace_name_is_reserved_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let a = root.join("kid");
    write(
        &default_config_path(root),
        "\
project: parent
children:
  kid:
    dir: ./kid
    prefix: team
workspaces:
  team.orchestrator:
    dir: ./solo
    runtime: claude
",
    );
    write(
        &default_config_path(&a),
        "project: kid\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    let err = validation_err(Config::load(default_config_path(root)).unwrap_err());
    match err {
        ValidationError::ReservedNameForChild { session, .. } => {
            assert_eq!(session, "team.orchestrator");
        }
        other => panic!("expected ReservedNameForChild, got {other:?}"),
    }
}

#[test]
fn valid_tree_passes_validation_and_loads() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let child = root.join("child");
    write(
        &default_config_path(root),
        "project: parent\nchildren:\n  sub:\n    dir: ./child\n    prefix: team\nworkspaces:\n  main:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&child),
        "project: child\nworkspaces:\n  worker:\n    dir: .\n    runtime: claude\n",
    );
    let cfg = Config::load(default_config_path(root)).expect("load");
    assert!(cfg.workspaces.contains_key("team.worker"));
}
