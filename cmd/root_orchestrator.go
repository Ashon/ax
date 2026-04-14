package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/workspace"
	"gopkg.in/yaml.v3"
)

type rootOrchestratorFlags struct {
	DisableRootOrchestrator bool `yaml:"disable_root_orchestrator,omitempty"`
}

func rootOrchestratorDisabled(cfgPath string) (bool, error) {
	if cfgPath == "" {
		return false, nil
	}

	data, err := os.ReadFile(cfgPath)
	if err != nil {
		return false, fmt.Errorf("read config %s: %w", cfgPath, err)
	}

	var flags rootOrchestratorFlags
	if err := yaml.Unmarshal(data, &flags); err != nil {
		return false, fmt.Errorf("parse config %s: %w", cfgPath, err)
	}
	return flags.DisableRootOrchestrator, nil
}

func reconcileRootOrchestratorState(cfgPath string) (bool, error) {
	disabled, err := rootOrchestratorDisabled(cfgPath)
	if err != nil {
		return false, err
	}
	if !disabled {
		return false, nil
	}

	if err := cleanupRootOrchestratorState(); err != nil {
		return false, err
	}
	return true, nil
}

func cleanupRootOrchestratorState() error {
	rootName := workspace.OrchestratorName("")
	if tmux.SessionExists(rootName) {
		if err := tmux.DestroySession(rootName); err != nil {
			return fmt.Errorf("destroy %s session: %w", rootName, err)
		}
	}

	orchDir, err := rootOrchestratorDir()
	if err != nil {
		return err
	}
	if err := cleanupRootOrchestratorArtifacts(orchDir); err != nil {
		return err
	}

	codexHome, err := agent.CodexHomePath(rootName, orchDir)
	if err != nil {
		return err
	}
	if err := os.RemoveAll(codexHome); err != nil {
		return fmt.Errorf("remove root codex home %s: %w", codexHome, err)
	}

	return nil
}

func rootOrchestratorDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolve home dir: %w", err)
	}
	return filepath.Join(home, ".ax", "orchestrator"), nil
}

func cleanupRootOrchestratorArtifacts(orchDir string) error {
	if err := workspace.RemoveMCPConfig(orchDir); err != nil {
		return fmt.Errorf("remove root mcp config: %w", err)
	}

	for _, runtimeName := range agent.SupportedNames() {
		file, err := agent.InstructionFile(runtimeName)
		if err != nil {
			return err
		}
		if err := os.Remove(filepath.Join(orchDir, file)); err != nil && !os.IsNotExist(err) {
			return fmt.Errorf("remove root instruction %s: %w", file, err)
		}
	}

	if err := os.RemoveAll(filepath.Join(orchDir, ".claude")); err != nil {
		return fmt.Errorf("remove root .claude dir: %w", err)
	}

	entries, err := os.ReadDir(orchDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("read root orchestrator dir %s: %w", orchDir, err)
	}
	if len(entries) == 0 {
		if err := os.Remove(orchDir); err != nil && !os.IsNotExist(err) {
			return fmt.Errorf("remove empty root orchestrator dir %s: %w", orchDir, err)
		}
	}
	return nil
}
