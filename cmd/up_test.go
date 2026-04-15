package cmd

import (
	"path/filepath"
	"testing"

	"github.com/ashon/ax/internal/config"
)

func TestUpPreparesArtifactsWithoutStartingSessions(t *testing.T) {
	rootDir := t.TempDir()
	configPathValue := filepath.Join(rootDir, ".ax", "config.yaml")
	writeTestConfig(t, configPathValue, `
project: sample
workspaces:
  worker:
    dir: ./worker
    runtime: claude
`)

	oldConfigPath := configPath
	oldSocketPath := socketPath
	oldEnsureDaemon := upEnsureDaemon
	oldEnsureWorkspaceArtifacts := upEnsureWorkspaceArtifacts
	oldRefreshOrchestratorArtifacts := upRefreshOrchestratorArtifacts
	oldRootOrchestratorDisabled := upRootOrchestratorDisabled
	t.Cleanup(func() {
		configPath = oldConfigPath
		socketPath = oldSocketPath
		upEnsureDaemon = oldEnsureDaemon
		upEnsureWorkspaceArtifacts = oldEnsureWorkspaceArtifacts
		upRefreshOrchestratorArtifacts = oldRefreshOrchestratorArtifacts
		upRootOrchestratorDisabled = oldRootOrchestratorDisabled
	})

	configPath = configPathValue
	socketPath = filepath.Join(rootDir, "daemon.sock")

	daemonEnsured := false
	var prepared []string
	orchestratorsPrepared := false

	upEnsureDaemon = func() error {
		daemonEnsured = true
		return nil
	}
	upEnsureWorkspaceArtifacts = func(name string, ws config.Workspace, socketPath, cfgPath string) error {
		prepared = append(prepared, name+":"+ws.Dir+":"+cfgPath)
		return nil
	}
	upRefreshOrchestratorArtifacts = func(node *config.ProjectNode, parentName, socketPath, cfgPath string) error {
		orchestratorsPrepared = true
		if node == nil || node.Name != "sample" {
			t.Fatalf("unexpected project node: %+v", node)
		}
		if parentName != "" {
			t.Fatalf("unexpected parent name %q", parentName)
		}
		if cfgPath != configPathValue {
			t.Fatalf("unexpected config path %q", cfgPath)
		}
		return nil
	}
	upRootOrchestratorDisabled = func(string) (bool, error) {
		return false, nil
	}

	if err := upCmd.RunE(upCmd, nil); err != nil {
		t.Fatalf("up command failed: %v", err)
	}

	if !daemonEnsured {
		t.Fatal("expected daemon to be ensured")
	}
	expectedWorkspace := filepath.Join(rootDir, "worker")
	if len(prepared) != 1 || prepared[0] != "worker:"+expectedWorkspace+":"+configPathValue {
		t.Fatalf("unexpected prepared workspaces: %+v", prepared)
	}
	if !orchestratorsPrepared {
		t.Fatal("expected orchestrator artifacts to be prepared")
	}
}
