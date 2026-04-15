package cmd

import (
	"fmt"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/workspace"
)

var (
	orchSessionExists     = tmux.SessionExists
	orchCreateEphemeral   = tmux.CreateEphemeralSession
	orchAttachSession     = tmux.AttachSession
)

// runRootOrchestrator prepares the root orchestrator workspace directory
// (instruction file + MCP config + sub-orchestrator sessions) and then
// launches the agent CLI inside an ephemeral tmux session. The session
// has no remain-on-exit so it is destroyed automatically when the user
// exits the agent. The user's terminal attaches to the session, which
// lets the daemon's wake scheduler deliver messages via send-keys.
func runRootOrchestrator(runtime string, runtimeArgs []string) error {
	runtime = agent.NormalizeRuntime(runtime)
	if _, err := agent.Get(runtime); err != nil {
		return err
	}

	cfgPath, err := resolveConfigPath()
	if err != nil {
		return err
	}
	disabled, err := rootOrchestratorDisabled(cfgPath)
	if err != nil {
		return err
	}
	if disabled {
		// `disable_root_orchestrator` only disables managed root session/state.
		// A user-triggered foreground CLI launch should still be able to act as
		// the root orchestrator, so we clear any stale managed state and then
		// regenerate root artifacts below.
		if err := cleanupRootOrchestratorState(); err != nil {
			return err
		}
	}
	tree, err := config.LoadTree(cfgPath)
	if err != nil {
		return fmt.Errorf("load config tree: %w", err)
	}

	// Override the root orchestrator runtime for this invocation so the
	// generated prompt and MCP config match the CLI the user asked for.
	// We intentionally mutate the in-memory tree only — the on-disk config
	// is untouched.
	tree.OrchestratorRuntime = runtime

	sp := daemon.ExpandSocketPath(socketPath)
	if !isDaemonRunning(sp) {
		if err := ensureDaemon(); err != nil {
			return fmt.Errorf("start daemon: %w", err)
		}
	}

	// Write root artifacts + start any sub-orchestrator sessions that are
	// missing. ensureOrchestrators is idempotent for already-running
	// sub-sessions and does not create a tmux session for the root node.
	// Direct launches must always materialize the root orchestrator, even
	// when managed root state is disabled in config.
	if err := ensureOrchestratorsWithSkipRoot(tree, sp, cfgPath, false); err != nil {
		return err
	}

	orchDir, err := orchestratorDir(tree)
	if err != nil {
		return fmt.Errorf("resolve orchestrator dir: %w", err)
	}

	selfName := workspace.OrchestratorName(tree.Prefix)

	// If an orchestrator session is already running, just attach.
	if orchSessionExists(selfName) {
		return orchAttachSession(selfName)
	}

	// Build an ax run-agent argv. The tmux command stays short because the
	// long --append-system-prompt injection happens inside the child process,
	// not on the tmux command line.
	axBin, err := agent.ResolveAxBinary()
	if err != nil {
		return fmt.Errorf("resolve ax binary: %w", err)
	}
	argv := []string{axBin, "run-agent", "--runtime", runtime, "--workspace", selfName, "--socket", sp, "--config", cfgPath}
	if len(runtimeArgs) > 0 {
		argv = append(argv, "--")
		argv = append(argv, runtimeArgs...)
	}

	// Create an ephemeral tmux session (no remain-on-exit). When the user
	// exits the agent CLI the session is destroyed automatically.
	if err := orchCreateEphemeral(selfName, orchDir, argv); err != nil {
		return fmt.Errorf("create orchestrator session: %w", err)
	}

	// Attach blocks until the user exits the agent and the session dies.
	return orchAttachSession(selfName)
}
