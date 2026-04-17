package cmd

import (
	"fmt"
	"os"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/mcpserver"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/workspace"
)

// ensureOrchestrators walks the project tree and makes sure each project's
// orchestrator artifacts (prompt + MCP config) exist. Sub-orchestrators
// (every non-root node) also get a long-running tmux session, since they
// need to be always-on for cross-project delegation. The root orchestrator
// is started on demand by `ax claude` / `ax codex` as an ephemeral tmux
// session using the artifacts written here.
func ensureOrchestrators(tree *config.ProjectNode, socketPath, cfgPath string) error {
	if tree == nil {
		return nil
	}
	skipRoot, err := reconcileRootOrchestratorState(cfgPath)
	if err != nil {
		return err
	}
	return ensureOrchestratorsWithSkipRoot(tree, socketPath, cfgPath, skipRoot)
}

func ensureOrchestratorsWithSkipRoot(tree *config.ProjectNode, socketPath, cfgPath string, skipRoot bool) error {
	if tree == nil {
		return nil
	}
	return createOrchestratorForNode(tree, "", socketPath, cfgPath, skipRoot)
}

func createOrchestratorForNode(node *config.ProjectNode, parentName, socketPath, cfgPath string, skipRoot bool) error {
	selfName := workspace.OrchestratorName(node.Prefix)
	isRoot := node.Prefix == ""

	if !(isRoot && skipRoot) {
		if err := workspace.EnsureOrchestrator(node, parentName, socketPath, cfgPath, !isRoot); err != nil {
			return err
		}
	}

	childParentName := selfName
	if isRoot && skipRoot {
		childParentName = ""
	}
	for _, child := range node.Children {
		if err := createOrchestratorForNode(child, childParentName, socketPath, cfgPath, skipRoot); err != nil {
			return err
		}
	}
	return nil
}

// orchestratorDir returns the directory that holds the orchestrator's
// instruction + mcp files. Root uses ~/.ax/orchestrator; sub-orchestrators
// live inside their project directory under .ax/orchestrator.
func orchestratorDir(node *config.ProjectNode) (string, error) {
	return workspace.OrchestratorDirForNode(node)
}

// destroyOrchestrators stops all orchestrator sessions in the tree.
func destroyOrchestrators(tree *config.ProjectNode) {
	if tree == nil {
		return
	}
	destroyOrchestratorForNode(tree)
}

func destroyOrchestratorForNode(node *config.ProjectNode) {
	for _, child := range node.Children {
		destroyOrchestratorForNode(child)
	}
	selfName := workspace.OrchestratorName(node.Prefix)
	if tmux.SessionExists(selfName) {
		tmux.DestroySession(selfName)
		fmt.Printf("  %s: stopped\n", selfName)
	}
}

// refreshOrchestratorTree is called after registering a new sub-project.
// It reloads the topmost config, regenerates all orchestrator prompt files
// so they mention the new child, creates any missing sub-orchestrator
// sessions, and — if the user happens to have a root orchestrator CLI
// (`ax claude`/`ax codex`) currently running — notifies it of the new
// child via the daemon.
func refreshOrchestratorTree(newChildName string) error {
	cfgPath, err := resolveConfigPath()
	if err != nil {
		return err
	}
	tree, err := config.LoadTree(cfgPath)
	if err != nil {
		return err
	}
	sp := daemonutil.ExpandSocketPath(socketPath)
	skipRoot, err := reconcileRootOrchestratorState(cfgPath)
	if err != nil {
		return err
	}

	// Only create sessions / send messages if the daemon is running
	if !isDaemonRunning(sp) {
		// Still regenerate prompt files so next ax up picks them up
		return writeOrchestratorPromptsOnly(tree, "", skipRoot)
	}

	if err := ensureOrchestrators(tree, sp, cfgPath); err != nil {
		return err
	}

	// Notify the root orchestrator so it can pick up the new sub-project
	rootName := workspace.OrchestratorName(tree.Prefix)
	if tmux.SessionExists(rootName) {
		client := mcpserver.NewDaemonClient(sp, "cli")
		if err := client.Connect(); err == nil {
			defer client.Close()
			msg := fmt.Sprintf(
				"New sub-project `%s` registered. Run list_agents/list_workspaces to see its workspaces and sub-orchestrator.",
				newChildName,
			)
			_, _ = client.SendMessage(rootName, msg, "")
		}
	}
	return nil
}

// writeOrchestratorPromptsOnly walks the tree and regenerates prompt files
// without touching tmux sessions. Used when the daemon isn't running.
func writeOrchestratorPromptsOnly(node *config.ProjectNode, parentName string, skipRoot bool) error {
	if node == nil {
		return nil
	}
	selfName := workspace.OrchestratorName(node.Prefix)
	isRoot := node.Prefix == ""
	if !(isRoot && skipRoot) {
		orchDir, err := orchestratorDir(node)
		if err != nil {
			return err
		}
		if err := os.MkdirAll(orchDir, 0o755); err != nil {
			return err
		}
		runtime := agent.NormalizeRuntime(node.OrchestratorRuntime)
		if err := workspace.WriteOrchestratorPrompt(orchDir, node, node.Prefix, parentName, runtime, socketPath); err != nil {
			return err
		}
	}
	childParentName := selfName
	if isRoot && skipRoot {
		childParentName = ""
	}
	for _, child := range node.Children {
		if err := writeOrchestratorPromptsOnly(child, childParentName, skipRoot); err != nil {
			return err
		}
	}
	return nil
}
