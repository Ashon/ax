package agent

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestPrepareCodexHomeSetsSharedReasoningEffortDefault(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := os.MkdirAll(filepath.Join(home, ".codex"), 0o755); err != nil {
		t.Fatalf("mkdir codex dir: %v", err)
	}
	baseConfig := "[projects.\"/tmp/existing\"]\ntrust_level = \"trusted\"\n"
	if err := os.WriteFile(filepath.Join(home, ".codex", "config.toml"), []byte(baseConfig), 0o644); err != nil {
		t.Fatalf("write base config: %v", err)
	}

	codexHome, err := PrepareCodexHome("ws", "/tmp/workspace", "/tmp/ax.sock", "/tmp/ax", filepath.Join(home, "missing.yaml"))
	if err != nil {
		t.Fatalf("PrepareCodexHome: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(codexHome, "config.toml"))
	if err != nil {
		t.Fatalf("read generated config: %v", err)
	}
	content := string(data)
	if !strings.Contains(content, "model_reasoning_effort = \"xhigh\"") {
		t.Fatalf("expected xhigh reasoning effort in generated config, got:\n%s", content)
	}
	if !strings.Contains(content, "[mcp_servers.ax]") {
		t.Fatalf("expected ax mcp server config in generated config, got:\n%s", content)
	}
	if !strings.Contains(content, "[projects.\"/tmp/workspace\"]") {
		t.Fatalf("expected workspace trust entry in generated config, got:\n%s", content)
	}
}

func TestPrepareCodexHomeUsesWorkspaceReasoningOverrideFromAxConfig(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := os.MkdirAll(filepath.Join(home, ".codex"), 0o755); err != nil {
		t.Fatalf("mkdir codex dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(home, ".codex", "config.toml"), []byte("model_reasoning_effort = \"medium\"\n"), 0o644); err != nil {
		t.Fatalf("write base config: %v", err)
	}

	configPath := filepath.Join(home, ".ax", "config.yaml")
	if err := os.MkdirAll(filepath.Dir(configPath), 0o755); err != nil {
		t.Fatalf("mkdir ax config dir: %v", err)
	}
	configYAML := `
codex_model_reasoning_effort: high
workspaces:
  ws:
    dir: .
    codex_model_reasoning_effort: low
`
	if err := os.WriteFile(configPath, []byte(configYAML), 0o644); err != nil {
		t.Fatalf("write ax config: %v", err)
	}

	codexHome, err := PrepareCodexHome("ws", home, "/tmp/ax.sock", "/tmp/ax", configPath)
	if err != nil {
		t.Fatalf("PrepareCodexHome: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(codexHome, "config.toml"))
	if err != nil {
		t.Fatalf("read generated config: %v", err)
	}
	content := string(data)
	if !strings.Contains(content, "model_reasoning_effort = \"low\"") {
		t.Fatalf("expected workspace reasoning override in generated config, got:\n%s", content)
	}
	if strings.Contains(content, "model_reasoning_effort = \"medium\"") {
		t.Fatalf("expected base codex reasoning effort to be overridden, got:\n%s", content)
	}
}

func TestPrepareCodexHomeForLaunchFreshRemovesStaleState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	codexHome, err := PrepareCodexHome("ws", "/tmp/workspace", "/tmp/ax.sock", "/tmp/ax", "")
	if err != nil {
		t.Fatalf("PrepareCodexHome: %v", err)
	}
	stalePath := filepath.Join(codexHome, "sessions", "stale.json")
	if err := os.MkdirAll(filepath.Dir(stalePath), 0o755); err != nil {
		t.Fatalf("mkdir stale dir: %v", err)
	}
	if err := os.WriteFile(stalePath, []byte("stale"), 0o644); err != nil {
		t.Fatalf("write stale file: %v", err)
	}

	refreshedHome, err := PrepareCodexHomeForLaunch("ws", "/tmp/workspace", "/tmp/ax.sock", "/tmp/ax", "", true)
	if err != nil {
		t.Fatalf("PrepareCodexHomeForLaunch: %v", err)
	}
	if refreshedHome != codexHome {
		t.Fatalf("expected refreshed home %q, got %q", codexHome, refreshedHome)
	}
	if _, err := os.Stat(stalePath); !os.IsNotExist(err) {
		t.Fatalf("expected stale state %q to be removed, stat err=%v", stalePath, err)
	}
	if _, err := os.Stat(filepath.Join(refreshedHome, "config.toml")); err != nil {
		t.Fatalf("expected regenerated config.toml, got stat err=%v", err)
	}
}

func TestUpsertTopLevelKeyReplacesExistingValue(t *testing.T) {
	content := "model = \"gpt-5.4\"\nmodel_reasoning_effort = \"medium\"\n[projects.\"/tmp/demo\"]\ntrust_level = \"trusted\"\n"
	updated := upsertTopLevelKey(content, "model_reasoning_effort", `"xhigh"`)
	if strings.Count(updated, "model_reasoning_effort = \"xhigh\"") != 1 {
		t.Fatalf("expected single updated reasoning effort entry, got:\n%s", updated)
	}
	if strings.Contains(updated, "model_reasoning_effort = \"medium\"") {
		t.Fatalf("expected previous reasoning effort value to be replaced, got:\n%s", updated)
	}
}
