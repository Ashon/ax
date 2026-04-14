package cmd

import (
	"fmt"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/workspace"
)

// runRootOrchestrator prepares the root orchestrator workspace directory
// (instruction file + MCP config + sub-orchestrator sessions) and then
// execs the given coding agent CLI in the foreground so the user interacts
// with it directly. The CLI registers to the daemon as the "orchestrator"
// workspace via the generated .mcp.json.
func runRootOrchestrator(runtime string) error {
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
	return agent.RunInDir(runtime, orchDir, selfName, sp, cfgPath)
}
