package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/config"
)

func TestPrepareRootClaudeLaunchWritesPromptAndMCP(t *testing.T) {
	home := t.TempDir()
	oldHome := os.Getenv("HOME")
	if err := os.Setenv("HOME", home); err != nil {
		t.Fatalf("set HOME: %v", err)
	}
	defer os.Setenv("HOME", oldHome)

	projectDir := t.TempDir()
	cfg := config.DefaultConfigForRuntime("demo", "codex")
	cfgPath := config.DefaultConfigPath(projectDir)
	if err := cfg.Save(cfgPath); err != nil {
		t.Fatalf("save config: %v", err)
	}

	tree, err := config.LoadTree(cfgPath)
	if err != nil {
		t.Fatalf("load tree: %v", err)
	}

	socketPath := filepath.Join(home, ".local", "state", "ax", "daemon.sock")
	spec, err := prepareRootClaudeLaunch(tree, cfgPath, socketPath, true)
	if err != nil {
		t.Fatalf("prepareRootClaudeLaunch: %v", err)
	}

	wantDir := filepath.Join(home, ".ax", "orchestrator")
	if spec.Dir != wantDir {
		t.Fatalf("expected orchestrator dir %q, got %q", wantDir, spec.Dir)
	}
	if spec.Workspace != "orchestrator" {
		t.Fatalf("expected workspace orchestrator, got %q", spec.Workspace)
	}

	promptPath := filepath.Join(spec.Dir, "CLAUDE.md")
	prompt, err := os.ReadFile(promptPath)
	if err != nil {
		t.Fatalf("read CLAUDE.md: %v", err)
	}
	if !strings.Contains(string(prompt), "# ax root orchestrator") {
		t.Fatalf("expected root orchestrator prompt, got %q", string(prompt))
	}

	mcpPath := filepath.Join(spec.Dir, ".mcp.json")
	mcpData, err := os.ReadFile(mcpPath)
	if err != nil {
		t.Fatalf("read .mcp.json: %v", err)
	}
	if !strings.Contains(string(mcpData), "\"ax\"") || !strings.Contains(string(mcpData), socketPath) {
		t.Fatalf("expected ax MCP server entry in %q", string(mcpData))
	}

	if _, err := os.Stat(filepath.Join(spec.Dir, ".claude")); err != nil {
		t.Fatalf("expected .claude dir to exist: %v", err)
	}
}
