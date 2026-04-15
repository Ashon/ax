package workspace

import (
	"fmt"
	"path/filepath"
	"testing"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
)

func TestManagerRestartRemovesStaleCodexHomeWithoutExistingSession(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	socketPath := "/tmp/ax.sock"
	configPath := filepath.Join(home, "project", ".ax", "config.yaml")
	dir := filepath.Join(home, "project", "worker")
	ws := config.Workspace{
		Dir:          dir,
		Runtime:      agent.RuntimeCodex,
		Instructions: "worker instructions",
	}

	if err := EnsureArtifacts("worker", ws, socketPath, configPath); err != nil {
		t.Fatalf("ensure initial artifacts: %v", err)
	}
	codexHome, err := agent.CodexHomePath("worker", dir)
	if err != nil {
		t.Fatalf("codex home path: %v", err)
	}
	staleFile := filepath.Join(codexHome, "stale-session.txt")
	writeTestFile(t, staleFile, "stale")

	restoreWorkspaceSessionStubs(t)
	workspaceSessionExists = func(string) bool { return false }
	createCalled := false
	workspaceCreateSessionWithArgsEnv = func(name, gotDir string, argv []string, env map[string]string) error {
		createCalled = true
		if name != "worker" {
			return fmt.Errorf("unexpected workspace %q", name)
		}
		if gotDir != dir {
			return fmt.Errorf("unexpected dir %q", gotDir)
		}
		return nil
	}

	manager := NewManager(socketPath, configPath)
	if err := manager.Restart("worker", ws); err != nil {
		t.Fatalf("restart workspace: %v", err)
	}

	if !createCalled {
		t.Fatal("expected restart to recreate the workspace session")
	}
	assertNotExists(t, staleFile)
	assertExists(t, filepath.Join(dir, ".mcp.json"))
	assertExists(t, filepath.Join(codexHome, "config.toml"))
}

func TestManagerRestartDestroysExistingSessionBeforeCreate(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	socketPath := "/tmp/ax.sock"
	configPath := filepath.Join(home, "project", ".ax", "config.yaml")
	dir := filepath.Join(home, "project", "worker")
	ws := config.Workspace{
		Dir:     dir,
		Runtime: agent.RuntimeClaude,
	}

	restoreWorkspaceSessionStubs(t)
	sessionExists := true
	destroyCalled := false
	createCalled := false

	workspaceSessionExists = func(string) bool { return sessionExists }
	workspaceDestroySession = func(string) error {
		destroyCalled = true
		sessionExists = false
		return nil
	}
	workspaceCreateSessionWithArgsEnv = func(string, string, []string, map[string]string) error {
		if sessionExists {
			return fmt.Errorf("create called before session reset")
		}
		createCalled = true
		return nil
	}

	manager := NewManager(socketPath, configPath)
	if err := manager.Restart("worker", ws); err != nil {
		t.Fatalf("restart workspace: %v", err)
	}

	if !destroyCalled {
		t.Fatal("expected restart to destroy the existing session")
	}
	if !createCalled {
		t.Fatal("expected restart to create a replacement session")
	}
}

func restoreWorkspaceSessionStubs(t *testing.T) {
	t.Helper()

	oldSessionExists := workspaceSessionExists
	oldCreateSessionWithEnv := workspaceCreateSessionWithEnv
	oldCreateSessionWithCommandEnv := workspaceCreateSessionWithCommandEnv
	oldCreateSessionWithArgsEnv := workspaceCreateSessionWithArgsEnv
	oldCreateSessionWithArgs := workspaceCreateSessionWithArgs
	oldDestroySession := workspaceDestroySession
	oldWakeSession := workspaceWakeSession

	t.Cleanup(func() {
		workspaceSessionExists = oldSessionExists
		workspaceCreateSessionWithEnv = oldCreateSessionWithEnv
		workspaceCreateSessionWithCommandEnv = oldCreateSessionWithCommandEnv
		workspaceCreateSessionWithArgsEnv = oldCreateSessionWithArgsEnv
		workspaceCreateSessionWithArgs = oldCreateSessionWithArgs
		workspaceDestroySession = oldDestroySession
		workspaceWakeSession = oldWakeSession
	})
}
