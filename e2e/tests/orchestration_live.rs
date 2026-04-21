//! Live orchestration scenarios driven end-to-end against a real
//! codex team. Every test here is opt-in via `AX_E2E_LIVE=1`; the
//! default `cargo test` flow skips them so CI and incremental loops
//! don't burn API credits.
//!
//! Scenario layout (`e2e/scenarios/<name>/`):
//!   * `.ax/config.yaml` — codex-runtime team definition
//!   * `prompt.txt` — initial user prompt fed to the root orchestrator
//!   * `validate.sh` — exit 0 when the scenario is considered solved
//!   * Any fixture files the agents need
//!
//! Run one with:
//!   `AX_E2E_LIVE=1 cargo test -p ax-e2e --test orchestration_live`

use std::path::PathBuf;
use std::time::Duration;

use ax_e2e::harness::{
    self, ax_down, ax_up, start_daemon, start_root_orchestrator, HarnessError, Sandbox,
};

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

/// Run one scenario end-to-end. The harness sets up an isolated
/// sandbox, builds the current checkout's `ax` binary, seeds codex
/// auth from the host, spins up the team, sends the prompt, then
/// waits up to `timeout` for `validate.sh` to pass while the
/// orchestrator pane stays idle for `settle_window`.
fn drive_scenario(
    name: &str,
    timeout: Duration,
    settle_window: Duration,
) -> Result<(), HarnessError> {
    let mut sandbox = Sandbox::new()?;
    let ax = sandbox.build_ax()?;
    sandbox.copy_scenario(&scenario_dir(name))?;
    let _daemon = start_daemon(&sandbox, &ax)?;
    ax_up(&sandbox, &ax)?;
    let session = start_root_orchestrator(&sandbox, &ax)?;
    session.wait_idle(Duration::from_secs(120))?;
    let prompt = std::fs::read_to_string(scenario_dir(name).join("prompt.txt"))?;
    session.send_prompt(prompt.trim())?;

    let result =
        harness::wait_for_settled_success(timeout, Duration::from_secs(10), settle_window, || {
            let validate_ok = harness::run_validate_script(&sandbox, "validate.sh").is_ok();
            validate_ok && session.looks_idle()
        });
    // Best-effort ax down so sessions don't leak between scenarios
    // inside a single test binary invocation.
    ax_down(&sandbox, &ax);
    result
}

#[test]
fn hello_workspace_l1() {
    if !live_enabled() {
        eprintln!("skipping: set AX_E2E_LIVE=1 to run live codex scenarios");
        return;
    }
    let result = drive_scenario(
        "hello_workspace",
        Duration::from_secs(15 * 60),
        Duration::from_secs(15),
    );
    if let Err(e) = result {
        panic!("hello_workspace scenario failed: {e}");
    }
}

#[test]
fn delegated_split_l2() {
    if !live_enabled() {
        eprintln!("skipping: set AX_E2E_LIVE=1 to run live codex scenarios");
        return;
    }
    // Two workers in parallel — give the orchestrator more settle
    // time than L1 because task wiring + two worker boots pushes the
    // wall-clock floor up.
    let result = drive_scenario(
        "delegated_split",
        Duration::from_secs(25 * 60),
        Duration::from_secs(20),
    );
    if let Err(e) = result {
        panic!("delegated_split scenario failed: {e}");
    }
}
