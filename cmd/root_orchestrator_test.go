package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/agent"
)

func TestRefreshSkipsRootOrchestratorWhenDisabled(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	rootDir := filepath.Join(home, "project")
	childDir := filepath.Join(rootDir, "child")

	writeTestConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
project: child
workspaces:
  worker:
    dir: .
`)
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeTestConfig(t, rootConfigPath, `
project: root
disable_root_orchestrator: true
workspaces:
  main:
    dir: .
children:
  child:
    dir: ./child
    prefix: team
`)

	rootOrchDir := filepath.Join(home, ".ax", "orchestrator")
	writeTestConfig(t, filepath.Join(rootOrchDir, "CLAUDE.md"), "stale root prompt\n")
	writeTestConfig(t, filepath.Join(rootOrchDir, ".mcp.json"), "{}\n")

	oldConfigPath := configPath
	oldSocketPath := socketPath
	oldRefreshRestart := refreshRestart
	oldRefreshStartMissing := refreshStartMissing
	t.Cleanup(func() {
		configPath = oldConfigPath
		socketPath = oldSocketPath
		refreshRestart = oldRefreshRestart
		refreshStartMissing = oldRefreshStartMissing
	})

	configPath = rootConfigPath
	socketPath = filepath.Join(home, "daemon.sock")
	refreshRestart = false
	refreshStartMissing = false

	if err := refreshCmd.RunE(refreshCmd, nil); err != nil {
		t.Fatalf("refresh: %v", err)
	}

	if _, err := os.Stat(rootOrchDir); !os.IsNotExist(err) {
		t.Fatalf("expected root orchestrator dir to be removed, got stat err=%v", err)
	}

	childPrompt := filepath.Join(childDir, ".ax", "orchestrator-team", "CLAUDE.md")
	data, err := os.ReadFile(childPrompt)
	if err != nil {
		t.Fatalf("read child prompt: %v", err)
	}
	content := string(data)
	if !strings.Contains(content, "# ax root orchestrator") {
		t.Fatalf("expected child prompt to become a root prompt when top-level root is disabled, got %q", content)
	}
	if strings.Contains(content, "상위 오케스트레이터") {
		t.Fatalf("did not expect parent orchestrator reference in promoted child prompt, got %q", content)
	}
}

func TestRunRootOrchestratorRejectsDisabledConfigAndCleansState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	rootDir := filepath.Join(home, "project")
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeTestConfig(t, rootConfigPath, `
project: root
disable_root_orchestrator: true
workspaces:
  main:
    dir: .
`)

	rootOrchDir := filepath.Join(home, ".ax", "orchestrator")
	writeTestConfig(t, filepath.Join(rootOrchDir, "CLAUDE.md"), "stale root prompt\n")
	writeTestConfig(t, filepath.Join(rootOrchDir, ".mcp.json"), "{}\n")

	codexHome, err := agent.CodexHomePath("orchestrator", rootOrchDir)
	if err != nil {
		t.Fatalf("resolve codex home: %v", err)
	}
	writeTestConfig(t, filepath.Join(codexHome, "config.toml"), "model = \"gpt-5\"\n")

	oldConfigPath := configPath
	oldSocketPath := socketPath
	t.Cleanup(func() {
		configPath = oldConfigPath
		socketPath = oldSocketPath
	})

	configPath = rootConfigPath
	socketPath = filepath.Join(home, "daemon.sock")

	err = runRootOrchestrator(agent.RuntimeClaude)
	if err == nil {
		t.Fatal("expected disabled root orchestrator error, got nil")
	}
	if !strings.Contains(err.Error(), "disable_root_orchestrator") {
		t.Fatalf("expected disable_root_orchestrator error, got %v", err)
	}

	if _, statErr := os.Stat(rootOrchDir); !os.IsNotExist(statErr) {
		t.Fatalf("expected root orchestrator dir removed, got stat err=%v", statErr)
	}
	if _, statErr := os.Stat(codexHome); !os.IsNotExist(statErr) {
		t.Fatalf("expected root codex home removed, got stat err=%v", statErr)
	}
}
