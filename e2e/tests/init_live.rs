//! Live scenarios that exercise `ax init`'s axis-selection prompt.
//! Each scenario copies a project fixture into the sandbox, runs
//! `ax init --codex --no-refresh --axis <x>`, and asserts the
//! setup agent wrote a config.yaml whose `# axis:` comment matches
//! the expected branch.
//!
//! Gated by `AX_E2E_LIVE=1` — requires `codex` + `~/.codex/auth.json`.
//!
//!   `AX_E2E_LIVE=1 cargo test -p ax-e2e --test=init_live`
//!
//! The daemon is NOT booted: `--no-refresh` tells init to skip the
//! ancestor orchestrator walk, so these scenarios isolate the
//! prompt/axis logic from the runtime lifecycle that the
//! `orchestration_live.rs` scenarios already cover.

use std::path::PathBuf;

use ax_e2e::harness::{self, run_ax_init, run_validate_script, HarnessError, Sandbox};

fn scenario_dir(name: &str) -> PathBuf {
    harness::repo_root()
        .join("e2e")
        .join("scenarios")
        .join(name)
}

fn live_enabled() -> bool {
    std::env::var("AX_E2E_LIVE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn drive_init_scenario(name: &str, axis: &str) -> Result<(), HarnessError> {
    let mut sandbox = Sandbox::new()?;
    let ax = sandbox.build_ax()?;
    sandbox.copy_scenario(&scenario_dir(name))?;
    // --no-refresh skips the ancestor-orchestrator walk so we
    //   don't need a daemon; --codex pins runtime; --axis carries
    //   the flag we're validating.
    run_ax_init(&sandbox, &ax, &["--no-refresh", "--codex", "--axis", axis])?;
    run_validate_script(&sandbox, "validate.sh")
}

fn drive_reconfigure_scenario(name: &str) -> Result<(), HarnessError> {
    let mut sandbox = Sandbox::new()?;
    let ax = sandbox.build_ax()?;
    sandbox.copy_scenario(&scenario_dir(name))?;
    run_ax_init(&sandbox, &ax, &["--reconfigure", "--no-refresh", "--codex"])?;
    run_validate_script(&sandbox, "validate.sh")
}

#[test]
fn init_role_auto_picks_role_axis_on_role_shaped_project() {
    if !live_enabled() {
        eprintln!("skipping: set AX_E2E_LIVE=1 to run live init scenarios");
        return;
    }
    if let Err(e) = drive_init_scenario("init_role_auto", "auto") {
        panic!("init_role_auto failed: {e}");
    }
}

#[test]
fn init_domain_auto_picks_domain_axis_on_domain_shaped_project() {
    if !live_enabled() {
        eprintln!("skipping: set AX_E2E_LIVE=1 to run live init scenarios");
        return;
    }
    if let Err(e) = drive_init_scenario("init_domain_auto", "auto") {
        panic!("init_domain_auto failed: {e}");
    }
}

#[test]
fn init_domain_force_role_overrides_observed_shape() {
    if !live_enabled() {
        eprintln!("skipping: set AX_E2E_LIVE=1 to run live init scenarios");
        return;
    }
    if let Err(e) = drive_init_scenario("init_domain_force_role", "role") {
        panic!("init_domain_force_role failed: {e}");
    }
}

#[test]
fn init_reconfigure_adds_workspace_for_new_directory() {
    if !live_enabled() {
        eprintln!("skipping: set AX_E2E_LIVE=1 to run live init scenarios");
        return;
    }
    if let Err(e) = drive_reconfigure_scenario("init_reconfigure_add") {
        panic!("init_reconfigure_add failed: {e}");
    }
}
