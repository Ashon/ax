package cmd

import (
	"os"
	"path/filepath"
	"strconv"
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

func TestRunRootOrchestratorAllowsDirectLaunchWhenDisabled(t *testing.T) {
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

	binDir := filepath.Join(home, "bin")
	claudePath := filepath.Join(binDir, "claude")
	writeTestConfig(t, claudePath, "#!/bin/sh\nexit 0\n")
	if err := os.Chmod(claudePath, 0o755); err != nil {
		t.Fatalf("chmod fake claude: %v", err)
	}
	t.Setenv("PATH", binDir+string(os.PathListSeparator)+os.Getenv("PATH"))

	socket := filepath.Join(home, "daemon.sock")
	writeTestConfig(t, socket, "")
	writeTestConfig(t, filepath.Join(home, "daemon.pid"), strconv.Itoa(os.Getpid()))

	oldConfigPath := configPath
	oldSocketPath := socketPath
	t.Cleanup(func() {
		configPath = oldConfigPath
		socketPath = oldSocketPath
	})

	configPath = rootConfigPath
	socketPath = socket

	if err := runRootOrchestrator(agent.RuntimeClaude); err != nil {
		t.Fatalf("run root orchestrator: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(rootOrchDir, "CLAUDE.md"))
	if err != nil {
		t.Fatalf("read root prompt: %v", err)
	}
	content := string(data)
	if !strings.Contains(content, "# ax root orchestrator") {
		t.Fatalf("expected direct launch to regenerate root prompt, got %q", content)
	}
	if strings.Contains(content, "stale root prompt") {
		t.Fatalf("expected stale prompt to be replaced, got %q", content)
	}
	if _, statErr := os.Stat(filepath.Join(rootOrchDir, ".mcp.json")); statErr != nil {
		t.Fatalf("expected root mcp config to exist, got stat err=%v", statErr)
	}
}
