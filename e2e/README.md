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
- Future: full-stack scenarios that spin up config trees + tmux
  sessions. These will be `#[ignore]`-gated so they don't run
  under `cargo test` unless you opt in with
  `cargo test -p ax-e2e -- --ignored`.

## What doesn't

- Pure logic tests live in `crates/<name>/tests/` next to the
  implementation.
- The MCP server's own rmcp-level integration suite is in
  `crates/ax-mcp-server/tests/`.
- Anything that spawns a real `claude` / `codex` binary belongs
  behind a `CLAUDE_E2E=1` / `CODEX_E2E=1` env gate (not wired
  yet; add when the first live test lands).

## Running

```sh
# Everything (default — same as cargo test --workspace)
cargo test

# Just this crate
cargo test -p ax-e2e

# Ignored live tests (when we add them)
cargo test -p ax-e2e -- --ignored
```
