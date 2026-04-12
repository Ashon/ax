package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/mcpserver"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/workspace"
)

// ensureOrchestrators walks the project tree and makes sure an orchestrator
// tmux session exists for each project (root + every sub-project).
func ensureOrchestrators(tree *config.ProjectNode, socketPath, cfgPath string) error {
	if tree == nil {
		return nil
	}
	return createOrchestratorForNode(tree, "", socketPath, cfgPath)
}

func createOrchestratorForNode(node *config.ProjectNode, parentName, socketPath, cfgPath string) error {
	selfName := workspace.OrchestratorName(node.Prefix)
	orchDir, err := orchestratorDir(node)
	if err != nil {
		return fmt.Errorf("resolve orchestrator dir for %s: %w", selfName, err)
	}

	runtime := agent.NormalizeRuntime(node.OrchestratorRuntime)
	if _, err := agent.Get(runtime); err != nil {
		return fmt.Errorf("invalid orchestrator runtime for %s: %w", selfName, err)
	}

	if err := os.MkdirAll(orchDir, 0o755); err != nil {
		return fmt.Errorf("create orchestrator dir %s: %w", orchDir, err)
	}
	// Pre-create .claude to skip trust prompt for claude runtime
	os.MkdirAll(filepath.Join(orchDir, ".claude"), 0o755)

	if err := workspace.WriteMCPConfig(orchDir, selfName, socketPath, cfgPath); err != nil {
		return fmt.Errorf("write %s mcp config: %w", selfName, err)
	}
	if err := workspace.WriteOrchestratorPrompt(orchDir, node, node.Prefix, parentName, runtime); err != nil {
		return fmt.Errorf("write %s prompt: %w", selfName, err)
	}

	if !tmux.SessionExists(selfName) {
		exe, err := os.Executable()
		if err != nil {
			return fmt.Errorf("resolve ax binary: %w", err)
		}
		if err := tmux.CreateSessionWithArgs(selfName, orchDir, []string{
			exe,
			"run-agent",
			"--runtime", runtime,
			"--workspace", selfName,
			"--socket", socketPath,
			"--config", cfgPath,
		}); err != nil {
			return fmt.Errorf("create %s session: %w", selfName, err)
		}
	}

	for _, child := range node.Children {
		if err := createOrchestratorForNode(child, selfName, socketPath, cfgPath); err != nil {
			return err
		}
	}
	return nil
}

// orchestratorDir returns the directory that holds the orchestrator's
// instruction + mcp files. Root uses ~/.ax/orchestrator; sub-orchestrators
// live inside their project directory under .ax/orchestrator.
func orchestratorDir(node *config.ProjectNode) (string, error) {
	if node.Prefix == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		return filepath.Join(home, ".ax", "orchestrator"), nil
	}
	safe := strings.ReplaceAll(node.Prefix, ".", "_")
	return filepath.Join(node.Dir, ".ax", "orchestrator-"+safe), nil
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
// sessions, and notifies the running root orchestrator of the new child.
func refreshOrchestratorTree(newChildName string) error {
	cfgPath, err := resolveConfigPath()
	if err != nil {
		return err
	}
	tree, err := config.LoadTree(cfgPath)
	if err != nil {
		return err
	}
	sp := daemon.ExpandSocketPath(socketPath)

	// Only create sessions / send messages if the daemon is running
	if !isDaemonRunning(sp) {
		// Still regenerate prompt files so next ax up picks them up
		return writeOrchestratorPromptsOnly(tree, "")
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
			_, _ = client.SendMessage(rootName, msg)
		}
	}
	return nil
}

// writeOrchestratorPromptsOnly walks the tree and regenerates prompt files
// without touching tmux sessions. Used when the daemon isn't running.
func writeOrchestratorPromptsOnly(node *config.ProjectNode, parentName string) error {
	if node == nil {
		return nil
	}
	selfName := workspace.OrchestratorName(node.Prefix)
	orchDir, err := orchestratorDir(node)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(orchDir, 0o755); err != nil {
		return err
	}
	runtime := agent.NormalizeRuntime(node.OrchestratorRuntime)
	if err := workspace.WriteOrchestratorPrompt(orchDir, node, node.Prefix, parentName, runtime); err != nil {
		return err
	}
	for _, child := range node.Children {
		if err := writeOrchestratorPromptsOnly(child, selfName); err != nil {
			return err
		}
	}
	return nil
}
