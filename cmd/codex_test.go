package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/config"
)

func TestPrepareRootCodexLaunchWritesPromptAndConfig(t *testing.T) {
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
	spec, err := prepareRootCodexLaunch(tree, cfgPath, socketPath, true)
	if err != nil {
		t.Fatalf("prepareRootCodexLaunch: %v", err)
	}

	wantDir := filepath.Join(home, ".ax", "orchestrator")
	if spec.Dir != wantDir {
		t.Fatalf("expected orchestrator dir %q, got %q", wantDir, spec.Dir)
	}
	if spec.Workspace != "orchestrator" {
		t.Fatalf("expected workspace orchestrator, got %q", spec.Workspace)
	}

	promptPath := filepath.Join(spec.Dir, "AGENTS.md")
	prompt, err := os.ReadFile(promptPath)
	if err != nil {
		t.Fatalf("read AGENTS.md: %v", err)
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

	codexHomeEntries, err := filepath.Glob(filepath.Join(home, ".ax", "codex", "orchestrator-*", "config.toml"))
	if err != nil {
		t.Fatalf("glob codex home configs: %v", err)
	}
	if len(codexHomeEntries) != 1 {
		t.Fatalf("expected exactly one codex home config, got %d (%v)", len(codexHomeEntries), codexHomeEntries)
	}

	codexConfig, err := os.ReadFile(codexHomeEntries[0])
	if err != nil {
		t.Fatalf("read codex config: %v", err)
	}
	if !strings.Contains(string(codexConfig), "[mcp_servers.ax]") {
		t.Fatalf("expected ax MCP config in codex home, got %q", string(codexConfig))
	}
	if !strings.Contains(string(codexConfig), "\"orchestrator\"") {
		t.Fatalf("expected orchestrator workspace in codex config, got %q", string(codexConfig))
	}
}
