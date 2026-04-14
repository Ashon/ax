package cmd

import (
	"fmt"
	"os"

	"github.com/ashon/ax/internal/config"
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
	orchDir, err := rootOrchestratorDir()
	if err != nil {
		return err
	}
	return workspace.CleanupOrchestratorState(rootName, orchDir)
}

func rootOrchestratorDir() (string, error) {
	return workspace.RootOrchestratorDir()
}

func cleanupRootOrchestratorArtifacts(orchDir string) error {
	return workspace.CleanupOrchestratorArtifacts(orchDir)
}

func rootOrchestratorVisible(node *config.ProjectNode) bool {
	if node == nil {
		return false
	}
	return !(node.Prefix == "" && node.DisableRootOrchestrator)
}
