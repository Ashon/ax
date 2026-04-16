# Live Codex E2E

This package contains an opt-in live end-to-end test for the `ax` orchestration
stack using the real `codex` CLI, `tmux`, and an isolated toy-project fixture.

The harness:

1. Builds the current `ax` binary from the checkout.
2. Copies a toy `tasknote` fixture into a temporary sandbox.
3. Uses isolated `HOME`, `XDG_STATE_HOME`, `TMUX_TMPDIR`, and daemon socket paths.
4. Runs `ax up` against the fixture.
5. Starts a root `codex` orchestrator in a dedicated tmux session.
6. Sends a single user prompt and waits for delegated work to complete.
7. Verifies `go test ./...` in both workspace modules plus `go build ./cmd/tasknote` in `cli`.

Run it locally with:

```bash
AX_E2E_LIVE=1 go test ./e2e -run TestCodexOrchestratorBuildsTasknoteFixture -v -timeout 45m
```

Requirements:

- `tmux` installed
- `codex` installed and already authenticated
- network access for the live Codex runtime
- enough local time budget for a real multi-agent build
