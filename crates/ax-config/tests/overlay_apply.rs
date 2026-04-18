//! Verify the managed overlay patches a loaded Config correctly and that
//! it is automatically applied when `experimental_mcp_team_reconfigure`
//! is turned on.

use std::fs;

use ax_config::{
    default_config_path, managed_overlay_path, Config, ManagedChildPatch, ManagedOverlay,
    ManagedPolicyOverlay, ManagedWorkspacePatch,
};

fn write(path: &std::path::Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn apply_to_adds_patches_and_respects_delete_flag() {
    let mut cfg = Config::default_for_runtime("demo", "claude");
    cfg.workspaces.insert(
        "legacy".to_owned(),
        ax_config::Workspace {
            dir: "./legacy".to_owned(),
            runtime: "codex".to_owned(),
            ..Default::default()
        },
    );

    let mut overlay = ManagedOverlay {
        policies: ManagedPolicyOverlay {
            orchestrator_runtime: Some("codex".to_owned()),
            disable_root_orchestrator: Some(true),
        },
        ..Default::default()
    };
    overlay.workspaces.insert(
        "legacy".to_owned(),
        ManagedWorkspacePatch {
            delete: true,
            ..Default::default()
        },
    );
    overlay.workspaces.insert(
        "main".to_owned(),
        ManagedWorkspacePatch {
            description: Some("Updated".to_owned()),
            runtime: Some("codex".to_owned()),
            ..Default::default()
        },
    );
    overlay.children.insert(
        "sub".to_owned(),
        ManagedChildPatch {
            dir: Some("./subproject".to_owned()),
            prefix: Some("team".to_owned()),
            ..Default::default()
        },
    );

    overlay.apply_to(&mut cfg);

    assert_eq!(cfg.orchestrator_runtime, "codex");
    assert!(cfg.disable_root_orchestrator);
    assert!(!cfg.workspaces.contains_key("legacy"), "delete wins");
    let main = cfg.workspaces.get("main").unwrap();
    assert_eq!(main.description, "Updated");
    assert_eq!(main.runtime, "codex");
    let sub = cfg.children.get("sub").unwrap();
    assert_eq!(sub.dir, "./subproject");
    assert_eq!(sub.prefix, "team");
}

#[test]
fn enabled_false_removes_workspace_without_explicit_delete() {
    let mut cfg = Config::default_for_runtime("demo", "claude");
    let mut overlay = ManagedOverlay::default();
    overlay.workspaces.insert(
        "main".to_owned(),
        ManagedWorkspacePatch {
            enabled: Some(false),
            ..Default::default()
        },
    );
    overlay.apply_to(&mut cfg);
    assert!(!cfg.workspaces.contains_key("main"));
}

#[test]
fn load_applies_overlay_when_experimental_flag_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &default_config_path(root),
        "project: demo\nexperimental_mcp_team_reconfigure: true\nworkspaces:\n  main:\n    dir: .\n    runtime: claude\n",
    );

    let overlay = ManagedOverlay {
        policies: ManagedPolicyOverlay {
            orchestrator_runtime: Some("codex".to_owned()),
            ..Default::default()
        },
        workspaces: [(
            "main".to_owned(),
            ManagedWorkspacePatch {
                description: Some("via overlay".to_owned()),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    overlay
        .save_for(default_config_path(root))
        .expect("save overlay");

    let cfg = Config::load(default_config_path(root)).expect("load");
    assert_eq!(cfg.orchestrator_runtime, "codex");
    assert_eq!(cfg.workspaces["main"].description, "via overlay");
}

#[test]
fn load_skips_overlay_when_flag_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Flag is NOT set; overlay on disk must be ignored.
    write(
        &default_config_path(root),
        "project: demo\nworkspaces:\n  main:\n    dir: .\n    runtime: claude\n",
    );
    let overlay = ManagedOverlay {
        policies: ManagedPolicyOverlay {
            orchestrator_runtime: Some("codex".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    overlay.save_for(default_config_path(root)).unwrap();

    let cfg = Config::load(default_config_path(root)).unwrap();
    // Default initialize_local leaves orchestrator_runtime empty; the
    // overlay patch never runs.
    assert_eq!(cfg.orchestrator_runtime, "");
}

#[test]
fn managed_overlay_path_always_lands_under_dot_ax() {
    let path = managed_overlay_path("/tmp/proj/.ax/config.yaml");
    assert!(path.ends_with(".ax/managed_overlay.yaml"));

    // Legacy ax.yaml layout still points the overlay at `<root>/.ax/`.
    let path = managed_overlay_path("/tmp/proj/ax.yaml");
    assert!(path.ends_with(".ax/managed_overlay.yaml"));
}

#[test]
fn empty_overlay_load_tolerates_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let cfg_path = default_config_path(root);
    write(&cfg_path, "project: demo\n");
    // No overlay file exists — should return default overlay.
    let overlay = ManagedOverlay::load_for(&cfg_path).expect("load");
    assert!(overlay.workspaces.is_empty());
    assert!(overlay.policies.orchestrator_runtime.is_none());
}
