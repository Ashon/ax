package workspace

import (
	"path/filepath"
	"slices"
	"testing"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
)

func TestManagedRunAgentArgsAppendsFreshFlag(t *testing.T) {
	args := managedRunAgentArgs("/tmp/ax", agent.RuntimeClaude, "worker", "/tmp/ax.sock", "/tmp/ax.yaml", true)
	if !slices.Contains(args, "--fresh") {
		t.Fatalf("expected --fresh in run-agent args, got %v", args)
	}
}

func TestManagerRestartPassesFreshFlagToRunAgent(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	oldSessionExists := workspaceSessionExists
	oldCreateSessionWithArgsEnv := workspaceCreateSessionWithArgsEnv
	oldDestroySession := workspaceDestroySession
	t.Cleanup(func() {
		workspaceSessionExists = oldSessionExists
		workspaceCreateSessionWithArgsEnv = oldCreateSessionWithArgsEnv
		workspaceDestroySession = oldDestroySession
	})

	workspaceSessionExists = func(string) bool { return false }
	workspaceDestroySession = func(string) error { return nil }

	var capturedArgs []string
	workspaceCreateSessionWithArgsEnv = func(workspace, dir string, argv []string, env map[string]string) error {
		capturedArgs = append([]string(nil), argv...)
		return nil
	}

	manager := NewManager("/tmp/ax.sock", filepath.Join(home, ".ax", "config.yaml"))
	if err := manager.Restart("worker", config.Workspace{
		Dir:     filepath.Join(home, "repo"),
		Runtime: agent.RuntimeClaude,
	}); err != nil {
		t.Fatalf("Restart: %v", err)
	}

	if !slices.Contains(capturedArgs, "--fresh") {
		t.Fatalf("expected restart to pass --fresh, got %v", capturedArgs)
	}
}
