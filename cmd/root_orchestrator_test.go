package cmd

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/agent"
)

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
	oldSessionExists := orchSessionExists
	oldCreateEphemeral := orchCreateEphemeral
	oldAttachSession := orchAttachSession
	t.Cleanup(func() {
		configPath = oldConfigPath
		socketPath = oldSocketPath
		orchSessionExists = oldSessionExists
		orchCreateEphemeral = oldCreateEphemeral
		orchAttachSession = oldAttachSession
	})

	configPath = rootConfigPath
	socketPath = socket

	// Stub tmux calls so the test doesn't need a real tmux server.
	orchSessionExists = func(string) bool { return false }
	orchCreateEphemeral = func(string, string, []string) error { return nil }
	orchAttachSession = func(string) error { return nil }

	if err := runRootOrchestrator(agent.RuntimeClaude, nil); err != nil {
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
