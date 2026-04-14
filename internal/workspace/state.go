package workspace

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/tmux"
)

func RootOrchestratorDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolve home dir: %w", err)
	}
	return filepath.Join(home, ".ax", "orchestrator"), nil
}

func OrchestratorDirForNode(node *config.ProjectNode) (string, error) {
	if node == nil {
		return "", fmt.Errorf("nil project node")
	}
	if node.Prefix == "" {
		return RootOrchestratorDir()
	}
	safe := strings.ReplaceAll(node.Prefix, ".", "_")
	return filepath.Join(node.Dir, ".ax", "orchestrator-"+safe), nil
}

func EnsureOrchestrator(node *config.ProjectNode, parentName, socketPath, configPath string, startSession bool) error {
	if node == nil {
		return nil
	}

	selfName := OrchestratorName(node.Prefix)
	isRoot := node.Prefix == ""
	orchDir, err := OrchestratorDirForNode(node)
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
	if err := os.MkdirAll(filepath.Join(orchDir, ".claude"), 0o755); err != nil {
		return fmt.Errorf("create orchestrator claude dir %s: %w", orchDir, err)
	}
	if err := WriteMCPConfig(orchDir, selfName, socketPath, configPath); err != nil {
		return fmt.Errorf("write %s mcp config: %w", selfName, err)
	}
	if runtime == agent.RuntimeCodex {
		if err := EnsureCodexConfig(orchDir, selfName, socketPath, configPath); err != nil {
			return fmt.Errorf("write %s codex config: %w", selfName, err)
		}
	}
	if err := WriteOrchestratorPrompt(orchDir, node, node.Prefix, parentName, runtime); err != nil {
		return fmt.Errorf("write %s prompt: %w", selfName, err)
	}

	if !isRoot && startSession && !tmux.SessionExists(selfName) {
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
			"--config", configPath,
		}); err != nil {
			return fmt.Errorf("create %s session: %w", selfName, err)
		}
	}

	return nil
}

func CleanupWorkspaceArtifacts(name, dir string) error {
	if strings.TrimSpace(dir) == "" {
		return nil
	}
	if err := RemoveMCPConfig(dir); err != nil {
		return fmt.Errorf("remove workspace mcp config: %w", err)
	}
	RemoveInstructions(dir)
	if err := agent.RemoveCodexHome(name, dir); err != nil {
		return err
	}
	return nil
}

func CleanupWorkspaceState(name, dir string) error {
	if tmux.SessionExists(name) {
		if err := tmux.DestroySession(name); err != nil {
			return fmt.Errorf("destroy %s session: %w", name, err)
		}
	}
	return CleanupWorkspaceArtifacts(name, dir)
}

func CleanupOrchestratorState(name, orchDir string) error {
	if tmux.SessionExists(name) {
		if err := tmux.DestroySession(name); err != nil {
			return fmt.Errorf("destroy %s session: %w", name, err)
		}
	}
	if err := CleanupOrchestratorArtifacts(orchDir); err != nil {
		return err
	}
	if err := agent.RemoveCodexHome(name, orchDir); err != nil {
		return err
	}
	return nil
}

func CleanupOrchestratorArtifacts(orchDir string) error {
	if strings.TrimSpace(orchDir) == "" {
		return nil
	}
	if err := RemoveMCPConfig(orchDir); err != nil {
		return fmt.Errorf("remove orchestrator mcp config: %w", err)
	}

	for _, runtimeName := range agent.SupportedNames() {
		file, err := agent.InstructionFile(runtimeName)
		if err != nil {
			return err
		}
		if err := os.Remove(filepath.Join(orchDir, file)); err != nil && !os.IsNotExist(err) {
			return fmt.Errorf("remove orchestrator instruction %s: %w", file, err)
		}
	}

	if err := os.RemoveAll(filepath.Join(orchDir, ".claude")); err != nil {
		return fmt.Errorf("remove orchestrator .claude dir: %w", err)
	}

	entries, err := os.ReadDir(orchDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("read orchestrator dir %s: %w", orchDir, err)
	}
	if len(entries) == 0 {
		if err := os.Remove(orchDir); err != nil && !os.IsNotExist(err) {
			return fmt.Errorf("remove empty orchestrator dir %s: %w", orchDir, err)
		}
	}

	return nil
}
