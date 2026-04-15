package workspace

import (
	"path/filepath"
	"testing"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
)

func TestEnsureArtifactsWritesManagedInstructionsWithoutCustomInstructions(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configPath := filepath.Join(home, "project", ".ax", "config.yaml")
	dir := filepath.Join(home, "project", "worker")
	ws := config.Workspace{
		Dir:     dir,
		Runtime: agent.RuntimeClaude,
	}

	if err := EnsureArtifacts("worker", ws, "/tmp/ax.sock", configPath); err != nil {
		t.Fatalf("ensure artifacts: %v", err)
	}

	assertExists(t, filepath.Join(dir, "CLAUDE.md"))
}
