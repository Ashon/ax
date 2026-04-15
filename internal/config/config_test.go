package config_test

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/config"
)

func TestLoadMergesChildrenRecursively(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "services", "invest")
	grandChildDir := filepath.Join(childDir, "monitoring")

	if err := os.MkdirAll(grandChildDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	writeConfig(t, filepath.Join(grandChildDir, ".ax", "config.yaml"), `
project: monitoring
workspaces:
  alerts:
    dir: .
    description: alerts agent
`)

	writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
project: invest
children:
  mon:
    dir: ./monitoring
workspaces:
  research:
    dir: .
    description: research agent
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
project: root
children:
  invest:
    dir: ./services/invest
workspaces:
  main:
    dir: .
    description: root agent
`)

	cfg, err := config.Load(rootConfigPath)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if cfg.Project != "root" {
		t.Fatalf("expected project root, got %q", cfg.Project)
	}

	if _, ok := cfg.Workspaces["main"]; !ok {
		t.Fatalf("expected root workspace main to exist")
	}
	if _, ok := cfg.Workspaces["invest.research"]; !ok {
		t.Fatalf("expected child workspace invest.research to exist")
	}
	if _, ok := cfg.Workspaces["invest.mon.alerts"]; !ok {
		t.Fatalf("expected grandchild workspace invest.mon.alerts to exist")
	}

	if got := cfg.Workspaces["invest.research"].Dir; got != childDir {
		t.Fatalf("expected invest.research dir %q, got %q", childDir, got)
	}
	if got := cfg.Workspaces["invest.mon.alerts"].Dir; got != grandChildDir {
		t.Fatalf("expected invest.mon.alerts dir %q, got %q", grandChildDir, got)
	}
}

func TestLoadRejectsCyclicChildren(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "child")

	if err := os.MkdirAll(childDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
children:
  child:
    dir: ./child
`)

	writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
children:
  root:
    dir: ..
`)

	if _, err := config.Load(rootConfigPath); err == nil {
		t.Fatal("expected cycle error, got nil")
	}
}

func TestLoadTreeRejectsSameChildUnderMultiplePrefixes(t *testing.T) {
	rootDir := t.TempDir()
	sharedDir := filepath.Join(rootDir, "shared")

	if err := os.MkdirAll(sharedDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	writeConfig(t, filepath.Join(sharedDir, ".ax", "config.yaml"), `
project: shared
workspaces:
  worker:
    dir: .
    description: shared worker
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
project: root
children:
  alpha:
    dir: ./shared
  beta:
    dir: ./shared
workspaces:
  main:
    dir: .
`)

	assertLoadersFailWithError(t, rootConfigPath, config.ErrDuplicateWorkspaceDir, sharedDir, "alpha.worker", "beta.worker")
}

func TestLoadTreeRejectsCyclicChildren(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "child")

	if err := os.MkdirAll(childDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
children:
  child:
    dir: ./child
`)

	writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
children:
  root:
    dir: ..
`)

	if _, err := config.LoadTree(rootConfigPath); !errors.Is(err, config.ErrCyclicChildren) {
		t.Fatalf("expected cycle error %v, got %v", config.ErrCyclicChildren, err)
	}
}

func TestLoadRejectsDuplicateSiblingChildPrefixes(t *testing.T) {
	rootDir := t.TempDir()
	firstDir := filepath.Join(rootDir, "first")
	secondDir := filepath.Join(rootDir, "second")

	writeConfig(t, filepath.Join(firstDir, ".ax", "config.yaml"), `
workspaces:
  main:
    dir: .
`)
	writeConfig(t, filepath.Join(secondDir, ".ax", "config.yaml"), `
workspaces:
  main:
    dir: .
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
children:
  first:
    dir: ./first
    prefix: team
  second:
    dir: ./second
    prefix: team
`)

	assertLoadersFailWithError(t, rootConfigPath, config.ErrDuplicateChildPrefix, "team", firstDir, secondDir)
}

func TestLoadRejectsDuplicateWorkspaceDirsInSameConfig(t *testing.T) {
	rootDir := t.TempDir()
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
workspaces:
  alpha:
    dir: ./shared
  beta:
    dir: ./shared
`)

	assertLoadersFailWithError(t, rootConfigPath, config.ErrDuplicateWorkspaceDir, filepath.Join(rootDir, "shared"), "alpha", "beta")
}

func TestLoadRejectsDuplicateWorkspaceDirsAcrossChildConfigs(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "child")
	if err := os.MkdirAll(childDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
workspaces:
  worker:
    dir: ../shared
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
workspaces:
  main:
    dir: ./shared
children:
  child:
    dir: ./child
`)

	assertLoadersFailWithError(t, rootConfigPath, config.ErrDuplicateWorkspaceDir, filepath.Join(rootDir, "shared"), "main", "child.worker")
}

func TestLoadRejectsDuplicateNestedChildPrefixes(t *testing.T) {
	rootDir := t.TempDir()
	firstDir := filepath.Join(rootDir, "first")
	grandChildDir := filepath.Join(firstDir, "grandchild")
	secondDir := filepath.Join(rootDir, "second")

	writeConfig(t, filepath.Join(grandChildDir, ".ax", "config.yaml"), `
workspaces:
  main:
    dir: .
`)
	writeConfig(t, filepath.Join(firstDir, ".ax", "config.yaml"), `
children:
  grandchild:
    dir: ./grandchild
`)
	writeConfig(t, filepath.Join(secondDir, ".ax", "config.yaml"), `
workspaces:
  main:
    dir: .
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
children:
  first:
    dir: ./first
  second:
    dir: ./second
    prefix: first.grandchild
`)

	assertLoadersFailWithError(t, rootConfigPath, config.ErrDuplicateChildPrefix, "first.grandchild", grandChildDir, secondDir)
}

func TestLoadRejectsReservedOrchestratorNameCollisions(t *testing.T) {
	t.Run("root workspace", func(t *testing.T) {
		rootDir := t.TempDir()
		rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
		writeConfig(t, rootConfigPath, `
workspaces:
  orchestrator:
    dir: .
`)

		assertLoadersFailWithError(t, rootConfigPath, config.ErrReservedNameCollision, "orchestrator", rootConfigPath)
	})

	t.Run("child workspace", func(t *testing.T) {
		rootDir := t.TempDir()
		childDir := filepath.Join(rootDir, "ops")

		writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
workspaces:
  orchestrator:
    dir: .
`)

		rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
		writeConfig(t, rootConfigPath, `
children:
  ops:
    dir: ./ops
`)

		assertLoadersFailWithError(t, rootConfigPath, config.ErrReservedNameCollision, "ops.orchestrator", childDir)
	})
}

func TestLoadAllowsRootWorkspaceNamedOrchestratorWhenRootDisabled(t *testing.T) {
	rootDir := t.TempDir()
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
disable_root_orchestrator: true
workspaces:
  orchestrator:
    dir: .
`)

	cfg, err := config.Load(rootConfigPath)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if !cfg.DisableRootOrchestrator {
		t.Fatal("expected disable_root_orchestrator to be preserved on load")
	}
	if _, ok := cfg.Workspaces["orchestrator"]; !ok {
		t.Fatal("expected root workspace named orchestrator to be allowed when root orchestrator is disabled")
	}

	tree, err := config.LoadTree(rootConfigPath)
	if err != nil {
		t.Fatalf("load tree: %v", err)
	}
	if !tree.DisableRootOrchestrator {
		t.Fatal("expected top-level tree node to mark disable_root_orchestrator")
	}
}

func TestLoadRejectsChildOrchestratorWorkspaceEvenWhenChildDisablesRoot(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "ops")

	writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
disable_root_orchestrator: true
workspaces:
  orchestrator:
    dir: .
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
children:
  ops:
    dir: ./ops
`)

	assertLoadersFailWithError(t, rootConfigPath, config.ErrReservedNameCollision, "ops.orchestrator", childDir)
}

func TestLoadTreeIgnoresChildDisableRootOrchestratorFlag(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "ops")

	writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
disable_root_orchestrator: true
workspaces:
  main:
    dir: .
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
disable_root_orchestrator: true
children:
  ops:
    dir: ./ops
`)

	tree, err := config.LoadTree(rootConfigPath)
	if err != nil {
		t.Fatalf("load tree: %v", err)
	}
	if !tree.DisableRootOrchestrator {
		t.Fatal("expected top-level disable_root_orchestrator to be set")
	}
	if len(tree.Children) != 1 {
		t.Fatalf("expected one child, got %d", len(tree.Children))
	}
	if tree.Children[0].DisableRootOrchestrator {
		t.Fatal("expected child disable_root_orchestrator to be ignored when loaded under a parent tree")
	}
}

func TestLoadIgnoresManagedOverlayWhenFeatureFlagIsOff(t *testing.T) {
	rootDir := t.TempDir()
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
workspaces:
  main:
    dir: .
`)
	writeManagedOverlay(t, rootConfigPath, `
workspaces: [
`)

	cfg, err := config.Load(rootConfigPath)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if _, ok := cfg.Workspaces["main"]; !ok {
		t.Fatal("expected base workspace to remain when feature flag is off")
	}
}

func TestLoadAppliesManagedOverlayWorkspaceAndPolicyChanges(t *testing.T) {
	rootDir := t.TempDir()
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
experimental_mcp_team_reconfigure: true
workspaces:
  main:
    dir: .
    description: base
`)
	writeManagedOverlay(t, rootConfigPath, `
policies:
  disable_root_orchestrator: true
workspaces:
  main:
    enabled: false
  helper:
    dir: ./helper
    description: managed helper
    runtime: codex
`)

	cfg, err := config.Load(rootConfigPath)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if !cfg.DisableRootOrchestrator {
		t.Fatal("expected managed policy overlay to disable root orchestrator")
	}
	if _, ok := cfg.Workspaces["main"]; ok {
		t.Fatal("expected managed overlay to disable base workspace main")
	}
	helper, ok := cfg.Workspaces["helper"]
	if !ok {
		t.Fatal("expected managed overlay to add helper workspace")
	}
	if helper.Runtime != "codex" {
		t.Fatalf("expected helper runtime codex, got %q", helper.Runtime)
	}
	if helper.Dir != filepath.Join(rootDir, "helper") {
		t.Fatalf("expected helper dir %q, got %q", filepath.Join(rootDir, "helper"), helper.Dir)
	}
}

func TestLoadRecursivelyAppliesManagedOverlayInChildConfig(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "child")
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")

	writeConfig(t, rootConfigPath, `
children:
  child:
    dir: ./child
workspaces:
  main:
    dir: .
`)
	writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
experimental_mcp_team_reconfigure: true
workspaces:
  worker:
    dir: .
`)
	writeManagedOverlay(t, filepath.Join(childDir, ".ax", "config.yaml"), `
workspaces:
  worker:
    delete: true
  helper:
    dir: .
    description: managed helper
`)

	cfg, err := config.Load(rootConfigPath)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if _, ok := cfg.Workspaces["child.worker"]; ok {
		t.Fatal("expected child worker workspace to be deleted by managed overlay")
	}
	if _, ok := cfg.Workspaces["child.helper"]; !ok {
		t.Fatal("expected child helper workspace from managed overlay")
	}
}

func TestLoadTreeAppliesManagedChildOverlay(t *testing.T) {
	rootDir := t.TempDir()
	oldChildDir := filepath.Join(rootDir, "old")
	newChildDir := filepath.Join(rootDir, "new")
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")

	writeConfig(t, rootConfigPath, `
experimental_mcp_team_reconfigure: true
children:
  old:
    dir: ./old
`)
	writeManagedOverlay(t, rootConfigPath, `
children:
  old:
    enabled: false
  new:
    dir: ./new
    prefix: nxt
`)
	writeConfig(t, filepath.Join(oldChildDir, ".ax", "config.yaml"), `
project: old
workspaces:
  main:
    dir: .
`)
	writeConfig(t, filepath.Join(newChildDir, ".ax", "config.yaml"), `
project: new
workspaces:
  main:
    dir: .
`)

	tree, err := config.LoadTree(rootConfigPath)
	if err != nil {
		t.Fatalf("load tree: %v", err)
	}
	if len(tree.Children) != 1 {
		t.Fatalf("expected one managed child, got %d", len(tree.Children))
	}
	if tree.Children[0].Alias != "new" {
		t.Fatalf("expected child alias new, got %q", tree.Children[0].Alias)
	}
	if tree.Children[0].Prefix != "nxt" {
		t.Fatalf("expected managed child prefix nxt, got %q", tree.Children[0].Prefix)
	}
}

func TestLoadRejectsManagedOverlayReservedNameCollisions(t *testing.T) {
	rootDir := t.TempDir()
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
experimental_mcp_team_reconfigure: true
workspaces:
  main:
    dir: .
`)
	writeManagedOverlay(t, rootConfigPath, `
workspaces:
  orchestrator:
    dir: .
`)

	assertLoadersFailWithError(t, rootConfigPath, config.ErrReservedNameCollision, "orchestrator", rootConfigPath)
}

func TestLoadRejectsMalformedChildConfig(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "broken")

	writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
workspaces:
  main: [
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
children:
  broken:
    dir: ./broken
`)

	assertLoadersFailWithError(t, rootConfigPath, nil, `load child "broken"`, childDir, "parse config")
}

func TestLoadRejectsUnreadableChildConfig(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "private")
	childConfigPath := filepath.Join(childDir, ".ax", "config.yaml")

	writeConfig(t, childConfigPath, `
workspaces:
  main:
    dir: .
`)

	if err := os.Chmod(childConfigPath, 0); err != nil {
		t.Fatalf("chmod %s: %v", childConfigPath, err)
	}
	t.Cleanup(func() {
		_ = os.Chmod(childConfigPath, 0o644)
	})

	if _, err := os.ReadFile(childConfigPath); err == nil {
		t.Skip("filesystem permissions do not block reads in this environment")
	}

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
children:
  private:
    dir: ./private
`)

	assertLoadersFailWithError(t, rootConfigPath, os.ErrPermission, `load child "private"`, childDir, "read config")
}

func TestLoadPreservesStaleMissingChildConfigBehavior(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "missing-child")
	if err := os.MkdirAll(childDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
workspaces:
  main:
    dir: .
children:
  missing:
    dir: ./missing-child
`)

	cfg, err := config.Load(rootConfigPath)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if len(cfg.Workspaces) != 1 {
		t.Fatalf("expected only root workspace to remain, got %d workspaces", len(cfg.Workspaces))
	}
	if _, ok := cfg.Workspaces["main"]; !ok {
		t.Fatalf("expected root workspace main to exist")
	}

	tree, err := config.LoadTree(rootConfigPath)
	if err != nil {
		t.Fatalf("load tree: %v", err)
	}
	if len(tree.Children) != 0 {
		t.Fatalf("expected stale missing child to be skipped, got %d children", len(tree.Children))
	}
}

func TestDefaultConfigUsesClaudeByDefault(t *testing.T) {
	cfg := config.DefaultConfig("demo")

	if cfg.OrchestratorRuntime != "claude" {
		t.Fatalf("expected orchestrator runtime claude, got %q", cfg.OrchestratorRuntime)
	}

	ws, ok := cfg.Workspaces["main"]
	if !ok {
		t.Fatal("expected main workspace to exist")
	}
	if ws.Runtime != "claude" {
		t.Fatalf("expected main workspace runtime claude, got %q", ws.Runtime)
	}
	if ws.CodexModelReasoningEffort != config.DefaultCodexReasoningEffort {
		t.Fatalf("expected main workspace codex reasoning effort %q, got %q", config.DefaultCodexReasoningEffort, ws.CodexModelReasoningEffort)
	}
}

func TestDefaultConfigForRuntimeUsesRequestedRuntime(t *testing.T) {
	cfg := config.DefaultConfigForRuntime("demo", "codex")

	if cfg.OrchestratorRuntime != "codex" {
		t.Fatalf("expected orchestrator runtime codex, got %q", cfg.OrchestratorRuntime)
	}

	ws, ok := cfg.Workspaces["main"]
	if !ok {
		t.Fatal("expected main workspace to exist")
	}
	if ws.Runtime != "codex" {
		t.Fatalf("expected main workspace runtime codex, got %q", ws.Runtime)
	}
	if ws.CodexModelReasoningEffort != config.DefaultCodexReasoningEffort {
		t.Fatalf("expected main workspace codex reasoning effort %q, got %q", config.DefaultCodexReasoningEffort, ws.CodexModelReasoningEffort)
	}
}

func TestLoadPropagatesProjectCodexReasoningEffortToWorkspaces(t *testing.T) {
	rootDir := t.TempDir()
	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
codex_model_reasoning_effort: high
workspaces:
  main:
    dir: .
`)

	cfg, err := config.Load(rootConfigPath)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if got := cfg.CodexReasoningEffortForWorkspace("main"); got != "high" {
		t.Fatalf("expected propagated workspace reasoning effort high, got %q", got)
	}
}

func TestLoadPreservesChildCodexReasoningEffortDefaults(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "child")
	if err := os.MkdirAll(childDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	writeConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
codex_model_reasoning_effort: low
workspaces:
  worker:
    dir: .
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeConfig(t, rootConfigPath, `
codex_model_reasoning_effort: high
children:
  child:
    dir: ./child
workspaces:
  main:
    dir: .
`)

	cfg, err := config.Load(rootConfigPath)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if got := cfg.CodexReasoningEffortForWorkspace("main"); got != "high" {
		t.Fatalf("expected root workspace reasoning effort high, got %q", got)
	}
	if got := cfg.CodexReasoningEffortForWorkspace("child.worker"); got != "low" {
		t.Fatalf("expected child workspace reasoning effort low, got %q", got)
	}
}

func TestCodexReasoningEffortForWorkspaceFallsBackToDefault(t *testing.T) {
	if got := (*config.Config)(nil).CodexReasoningEffortForWorkspace("missing"); got != config.DefaultCodexReasoningEffort {
		t.Fatalf("expected default reasoning effort %q, got %q", config.DefaultCodexReasoningEffort, got)
	}
}

func writeConfig(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir %s: %v", filepath.Dir(path), err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func writeManagedOverlay(t *testing.T, configPath, content string) {
	t.Helper()
	writeConfig(t, config.ManagedOverlayPath(configPath), content)
}

func assertLoadersFailWithError(t *testing.T, path string, want error, contains ...string) {
	t.Helper()

	loaders := []struct {
		name string
		fn   func(string) error
	}{
		{
			name: "Load",
			fn: func(path string) error {
				_, err := config.Load(path)
				return err
			},
		},
		{
			name: "LoadTree",
			fn: func(path string) error {
				_, err := config.LoadTree(path)
				return err
			},
		},
	}

	for _, loader := range loaders {
		err := loader.fn(path)
		if want != nil && !errors.Is(err, want) {
			t.Fatalf("%s: expected error %v, got %v", loader.name, want, err)
		}
		if want == nil && err == nil {
			t.Fatalf("%s: expected an error, got nil", loader.name)
		}
		for _, fragment := range contains {
			if !strings.Contains(err.Error(), fragment) {
				t.Fatalf("%s: expected error %q to contain %q", loader.name, err.Error(), fragment)
			}
		}
	}
}
