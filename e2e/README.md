# ax-e2e

End-to-end and cross-crate integration tests for ax. Lives as a
workspace member (`ax-e2e`) next to `crates/` so `cargo test
--workspace` picks it up automatically.

## What lives here

- **Cross-crate wire smoke tests** — boot `ax-daemon` in-process,
  drive it through the same Unix-socket envelope protocol the CLI
  and MCP server use, and assert that registration + message +
  task flows end-to-end. Catches regressions that single-crate
  unit tests miss.
- **Config + runtime safety caps** — `config_safety_caps.rs`
  exercises orchestrator-depth / children-per-node / concurrent-agent
  caps through the public `Config::load` + `ensure_dispatch_target`
  paths with a fake tmux.
- **Live orchestration scenarios** — `tests/orchestration_live.rs`
  drives a real codex team against a fixture in
  `scenarios/<name>/`, builds the current checkout's `ax` binary,
  seeds an isolated HOME/XDG_STATE_HOME/TMUX_TMPDIR, sends a prompt
  to the root orchestrator, and waits for the scenario's
  `validate.sh` to pass. **Gated by `AX_E2E_LIVE=1`** so the default
  `cargo test` flow stays offline.

## Live scenario layout

```
scenarios/<name>/
├── .ax/config.yaml     # codex-runtime team definition
├── SPEC.md             # human-readable description (not consumed by agents)
├── prompt.txt          # initial user prompt sent to the root orchestrator
├── validate.sh         # exit 0 = scenario solved; runs at the project root
└── <workspace dirs>/   # seeded fixture tree copied into the sandbox
```

Current scenarios:

- `hello_workspace` — L1, single workspace, trivial file-write task.
- `delegated_split` — L2, orchestrator fans out two parallel tasks
  to `alpha` + `beta` via `start_task`.

Add a new scenario by dropping a directory under `scenarios/` and
registering a test function in `tests/orchestration_live.rs` that
calls `drive_scenario("<name>", timeout, settle_window)`.

## Running

```sh
# Everything that doesn't need a real codex/tmux (fast, offline)
cargo test

# Just this crate
cargo test -p ax-e2e

# Live codex scenarios (requires host codex auth + tmux)
AX_E2E_LIVE=1 cargo test -p ax-e2e --test orchestration_live -- --nocapture
```

Live scenarios require:

- `tmux` and `codex` on PATH
- `~/.codex/auth.json` (host codex login) — symlinked into the
  sandbox so the test authenticates with your account but stays
  otherwise isolated
- Real network access and an account with enough time budget; the
  harness caps at 15–25 min per scenario
