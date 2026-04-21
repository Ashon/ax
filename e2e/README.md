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

## Non-live integration tests

These files run under the default `cargo test` flow. They boot
`ax-daemon` in-process against a tempdir socket — no tmux, no codex,
no network — so they catch wire-level regressions cheaply on every
commit.

### `daemon_roundtrip.rs` — baseline register + message

Boots the daemon, opens two sync Unix-socket clients, registers both
as workspaces, and verifies a `send_message` from one lands in the
other's inbox with the expected sender/recipient/body. This is the
minimum shape the CLI and MCP server drive in production.

| Test | What it asserts |
|------|-----------------|
| `daemon_roundtrip_send_message_lands_in_recipient_inbox` | A round-trip `SendMessage` envelope reaches the recipient's `ReadMessages` result with intact `from`/`to`/`content`. |

### `config_safety_caps.rs` — dispatch guard rails

Drives `Config::load` + `ensure_dispatch_target` through a fake tmux
backend to make sure the orchestrator-depth, children-per-node, and
concurrent-agent caps are enforced before any real dispatch work
runs. Protects against config regressions that would silently let a
workspace tree grow past safe bounds.

### `usage_probe.rs` — telemetry reachability

Validates that the daemon surfaces usage data (token trends, recent
MCP tool activity) through the wire in the shapes the TUI/CLI
consume. Catches serde drift in the usage response types.

### `task_lifecycle_roundtrip.rs` — task terminal-status push

Wire-level coverage of the task-lifecycle improvements shipped in
the `autoresearch/stale-lifecycle` run. The MCP-server integration
tests drive the typed methods; these tests drive the same flows
through the envelope protocol, proving the push-notification plumbing
works without the MCP layer in the loop.

| Test | What it asserts |
|------|-----------------|
| `completed_task_pushes_notification_into_creator_inbox` | When a worker updates its task to `Completed` with a contract-compliant marker, the creator's inbox receives a `[task-completed]` system message without the creator polling anything. |
| `failed_task_pushes_notification_with_reason_to_creator` | `Failed` transitions push `[task-failed]` along the same channel, with the failure reason inlined so the creator can triage without `get_task`. |
| `completion_without_marker_enqueues_reminder_into_worker_inbox` | When a worker's `Completed` transition is rejected (missing leftover-scope marker), the daemon returns the contract error AND enqueues a durable `[task-completion-rejected]` reminder into the worker's own inbox so the next `read_messages` resurfaces the remediation. |
| `self_assigned_task_completion_does_not_push_creator_notification` | When `creator == assignee`, the terminal-status push is suppressed so a single agent working its own backlog does not spam itself. |

### `peer_awareness_roundtrip.rs` — WorkspaceInfo liveness fields

Wire-level coverage of the peer-awareness fields added in the
`autoresearch/peer-awareness` run. Every field must survive the
serde roundtrip with the expected defaults, so orchestrators can
reason about peers from `list_workspaces` alone.

| Test | What it asserts |
|------|-----------------|
| `list_workspaces_returns_liveness_timestamps_for_registered_peers` | Each registered peer carries a non-null `last_activity_at`, `connected_at`, and a strictly positive `connection_generation`, and two concurrent peers receive distinct generations. |
| `list_workspaces_reports_active_task_count_and_current_task_id` | `active_task_count` counts every non-terminal task assigned to the peer (pending + in-progress). `current_task_id` only surfaces when the peer has at least one task in the `InProgress` state. |
| `list_workspaces_carries_declared_idle_timeout_through_the_wire` | The `idle_timeout_seconds` declared at `Register` time is preserved end-to-end; peers that did not declare one surface `0` so the field is unambiguous. |
| `connection_generation_bumps_on_reregister_over_the_wire` | Disconnecting and re-registering the same workspace name yields a strictly greater `connection_generation` — the cache-invalidation signal callers need to detect a peer restart. |

### `multi_agent_collaboration_roundtrip.rs` — end-to-end collaboration scenarios

Full multi-peer workflows driven by real threads concurrently against
a single in-process daemon. Each scenario composes nearly every
primitive the stale-lifecycle and peer-awareness runs touched, so a
regression in any one layer shows up here as a clear narrative
failure. Read these first when you want to understand how an agent
team is *supposed* to coordinate through the daemon.

#### Scenario 1: happy-path fan-out delivery

| Test | Shape | What it proves |
|------|-------|----------------|
| `three_specialists_deliver_coordinated_build_through_orch_push` | `orch` creates three tasks. `compiler` and `docs` run in parallel threads: each promotes its task to `InProgress`, publishes its output via `set_shared_value`, sends an `artifact-ready` message to `tests`, and reports `Completed` with the contract marker. `tests` runs on a third thread, gates on receiving both sibling ready-signals, reads both shared values, publishes the integrated artifact, and reports `Completed`. | ① All three `[task-completed]` pushes land in `orch`'s inbox without any polling. ② `list_workspaces` at the end shows `active_task_count=0` and `current_task_id=None` for every worker. ③ The integrated artifact contains both upstream payloads. |

Primitives exercised in one run: `create_task` × 3, `update_task`
state transitions, Completion Reporting Contract marker, `set_shared_value`
/ `get_shared_value`, `send_message` for sibling signalling,
`read_messages` for dependency gating, terminal-status push,
`list_workspaces` liveness fields.

#### Scenario 2: partial-failure escalation

| Test | Shape | What it proves |
|------|-------|----------------|
| `partial_failure_escalates_through_distinct_terminal_pushes` | Same fan-out shape, but `docs` hits a hard failure and reports `Failed` without publishing. `tests` polls briefly, notices `docs` never announced, sends a `[task-blocked-help]` message to `docs`, and transitions to `Blocked`. | ① All three terminal states push distinguishable messages into `orch`'s inbox: `[task-completed]` (compile), `[task-failed]` (docs), `[task-blocked]` (tests). ② The help-request message survives delivery even after `docs`'s task failed (peer's socket stays registered). ③ Peer-awareness asymmetry holds: `Completed`/`Failed` peers report `active_task_count=0` and `current_task_id=None`, but `Blocked` still counts as an open task so the orchestrator knows someone is still on the hook. |

This scenario is deliberately adversarial: without the
terminal-status push, without the `Blocked` / `Failed` distinction in
the push headers, or without the help-request delivery staying
functional across a failed peer, the test narrative visibly breaks.

### Adding a new non-live test

Follow the `SyncClient` / `spawn_daemon` pattern already in place —
each test file is self-contained with its own helper so test binaries
stay independently compilable. Write the scenario narrative in the
module-level `//!` header; the table in this README should summarise
it in one row so readers can find the right file from the behaviour
they are chasing.

## Live scenario layout

```
scenarios/<name>/
├── .ax/config.yaml     # codex-runtime team definition
├── SPEC.md             # human-readable description (not consumed by agents)
├── prompt.txt          # initial user prompt sent to the root orchestrator
├── validate.sh         # exit 0 = scenario solved; runs at the project root
└── <workspace dirs>/   # seeded fixture tree copied into the sandbox
```

Current orchestration scenarios (`tests/orchestration_live.rs`):

- `hello_workspace` — L1, single workspace, trivial file-write task.
- `delegated_split` — L2, orchestrator fans out two parallel tasks
  to `alpha` + `beta` via `start_task`.

Current init scenarios (`tests/init_live.rs`) — exercise the
Conway's-Law axis-selection prompt from `ax init`:

- `init_role_auto` — role-shaped project (frontend/backend/infra)
  with `--axis auto`; expects a role- or hybrid-axis config.
- `init_domain_auto` — domain-shaped project (users/orders/inventory)
  with `--axis auto`; expects a domain- or hybrid-axis config.
- `init_domain_force_role` — domain-shaped project with `--axis role`
  forced; expects the agent to override the observed shape and
  produce a role-axis config.
- `init_reconfigure_add` — pre-seeded role-axis config with
  frontend + backend; a new `infra/` directory is present. Runs
  `ax init --reconfigure` and expects the axis to be preserved, a
  `# reconfigured:` trail comment added, and a new workspace for
  infra to appear.

Add a new scenario by dropping a directory under `scenarios/` and
registering a test function in `tests/orchestration_live.rs` that
calls `drive_scenario("<name>", timeout, settle_window)`.

## Running

```sh
# Everything that doesn't need a real codex/tmux (fast, offline)
cargo test

# Just this crate
cargo test -p ax-e2e

# Live codex orchestration scenarios (requires host codex auth + tmux)
AX_E2E_LIVE=1 cargo test -p ax-e2e --test orchestration_live -- --nocapture

# Live init axis scenarios (requires host codex auth; no tmux needed)
AX_E2E_LIVE=1 cargo test -p ax-e2e --test init_live -- --nocapture
```

Live scenarios require:

- `tmux` and `codex` on PATH
- `~/.codex/auth.json` (host codex login) — symlinked into the
  sandbox so the test authenticates with your account but stays
  otherwise isolated
- Real network access and an account with enough time budget; the
  harness caps at 15–25 min per scenario
