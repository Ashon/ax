//! Structural validation coverage. Errors wrap into `TreeError::Validation`
//! and are surfaced through `Config::load`'s pre-pass.

use std::fs;
use std::path::Path;

use ax_config::{default_config_path, Config, TreeError, ValidationError};

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// Unwrap the boxed `ValidationError` out of a failed `Config::load`,
/// peeling `TreeError::Child` chains so deeply-nested validation
/// failures surface the original error kind.
fn validation_err(err: TreeError) -> ValidationError {
    let mut cur = err;
    loop {
        cur = match cur {
            TreeError::Validation(boxed) => return *boxed,
            TreeError::Child { source, .. } => *source,
            other => panic!("expected TreeError::Validation, got {other:?}"),
        };
    }
}

#[test]
fn duplicate_workspace_dir_is_allowed_for_role_axis_overlap() {
    // Role-axis projects commonly declare multiple workspaces over
    // the same subtree — e.g. docs/qa/implementation all operating
    // on the repo root. The validator must allow this: downstream
    // paths (tmux session, codex home, dispatch) are keyed on
    // workspace name, and telemetry attribution by cwd degrades
    // gracefully when a dir is shared.
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
    let cfg = Config::load(default_config_path(root)).expect("duplicate dirs must load");
    assert_eq!(cfg.workspaces.len(), 2);
    assert!(cfg.workspaces.contains_key("a"));
    assert!(cfg.workspaces.contains_key("b"));
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
fn orchestrator_depth_cap_rejects_trees_past_the_default() {
    // 4 nested configs = depth 0/1/2/3/4; the depth-4 node should
    // trip the default cap of 3.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let l1 = root.join("l1");
    let l2 = l1.join("l2");
    let l3 = l2.join("l3");
    let l4 = l3.join("l4");
    write(
        &default_config_path(root),
        "project: r\nchildren:\n  a:\n    dir: ./l1\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l1),
        "project: l1\nchildren:\n  b:\n    dir: ./l2\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l2),
        "project: l2\nchildren:\n  c:\n    dir: ./l3\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l3),
        "project: l3\nchildren:\n  d:\n    dir: ./l4\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l4),
        "project: l4\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    let err = validation_err(Config::load(default_config_path(root)).unwrap_err());
    match err {
        ValidationError::OrchestratorDepthExceeded { depth, cap, .. } => {
            assert_eq!(depth, 4);
            assert_eq!(cap, 3);
        }
        other => panic!("expected OrchestratorDepthExceeded, got {other:?}"),
    }
}

#[test]
fn orchestrator_depth_cap_of_zero_disables_the_check() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let l1 = root.join("l1");
    let l2 = l1.join("l2");
    let l3 = l2.join("l3");
    let l4 = l3.join("l4");
    write(
        &default_config_path(root),
        "project: r\nmax_orchestrator_depth: 0\nchildren:\n  a:\n    dir: ./l1\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l1),
        "project: l1\nchildren:\n  b:\n    dir: ./l2\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l2),
        "project: l2\nchildren:\n  c:\n    dir: ./l3\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l3),
        "project: l3\nchildren:\n  d:\n    dir: ./l4\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l4),
        "project: l4\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    let cfg = Config::load(default_config_path(root)).expect("unbounded depth loads");
    assert!(cfg.workspaces.contains_key("a.b.c.d.w"));
}

#[test]
fn children_per_node_cap_rejects_oversized_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut yaml = String::from("project: r\nmax_children_per_node: 2\nchildren:\n");
    for name in ["k1", "k2", "k3"] {
        use std::fmt::Write as _;
        writeln!(yaml, "  {name}:\n    dir: ./{name}").unwrap();
        write(
            &default_config_path(root.join(name).as_path()),
            &format!("project: {name}\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n"),
        );
    }
    yaml.push_str("workspaces:\n  main:\n    dir: .\n    runtime: claude\n");
    write(&default_config_path(root), &yaml);
    let err = validation_err(Config::load(default_config_path(root)).unwrap_err());
    match err {
        ValidationError::ChildrenPerNodeExceeded { count, cap, .. } => {
            assert_eq!(count, 3);
            assert_eq!(cap, 2);
        }
        other => panic!("expected ChildrenPerNodeExceeded, got {other:?}"),
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
