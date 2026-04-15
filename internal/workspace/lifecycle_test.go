package workspace

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
)

func TestStopNamedTargetStopsWorkspaceWithoutDeletingArtifacts(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	socketPath := "/tmp/ax.sock"
	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: codex\n")
	dir := filepath.Join(home, "worker")
	ws := config.Workspace{Dir: dir, Runtime: agent.RuntimeCodex}

	if err := EnsureArtifacts("worker", ws, socketPath, configPath); err != nil {
		t.Fatalf("ensure artifacts: %v", err)
	}
	codexHome, err := agent.CodexHomePath("worker", dir)
	if err != nil {
		t.Fatalf("codex home path: %v", err)
	}
	staleFile := filepath.Join(codexHome, "keep-me.txt")
	writeTestFile(t, staleFile, "persist")

	restoreWorkspaceSessionStubs(t)
	sessionExists := true
	destroyCalled := false
	workspaceSessionExists = func(name string) bool {
		return name == "worker" && sessionExists
	}
	workspaceDestroySession = func(name string) error {
		destroyCalled = true
		sessionExists = false
		if name != "worker" {
			return fmt.Errorf("unexpected workspace %q", name)
		}
		return nil
	}

	target, err := StopNamedTarget(socketPath, configPath, "worker")
	if err != nil {
		t.Fatalf("stop named target: %v", err)
	}

	if target.Kind != LifecycleTargetWorkspace {
		t.Fatalf("target kind = %q, want workspace", target.Kind)
	}
	if !destroyCalled {
		t.Fatal("expected workspace session to be destroyed")
	}
	assertExists(t, filepath.Join(dir, ".mcp.json"))
	assertExists(t, staleFile)
}

func TestStartNamedTargetStartsMissingWorkspaceByExactName(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	socketPath := "/tmp/ax.sock"
	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n")

	restoreWorkspaceSessionStubs(t)
	sessionExists := false
	created := false
	workspaceSessionExists = func(name string) bool {
		return name == "worker" && sessionExists
	}
	workspaceCreateSessionWithArgsEnv = func(name, dir string, argv []string, env map[string]string) error {
		created = true
		sessionExists = true
		if name != "worker" {
			return fmt.Errorf("unexpected workspace %q", name)
		}
		if dir != filepath.Join(home, "worker") {
			return fmt.Errorf("unexpected dir %q", dir)
		}
		return nil
	}

	target, err := StartNamedTarget(socketPath, configPath, "worker")
	if err != nil {
		t.Fatalf("start named target: %v", err)
	}

	if target.Kind != LifecycleTargetWorkspace {
		t.Fatalf("target kind = %q, want workspace", target.Kind)
	}
	if !created {
		t.Fatal("expected missing workspace session to be created")
	}
}

func TestRestartNamedTargetRecyclesWorkspaceSession(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	socketPath := "/tmp/ax.sock"
	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n")

	restoreWorkspaceSessionStubs(t)
	sessionExists := true
	var steps []string
	workspaceSessionExists = func(name string) bool {
		return name == "worker" && sessionExists
	}
	workspaceDestroySession = func(name string) error {
		steps = append(steps, "destroy:"+name)
		sessionExists = false
		return nil
	}
	workspaceCreateSessionWithArgsEnv = func(name, dir string, argv []string, env map[string]string) error {
		steps = append(steps, "create:"+name)
		sessionExists = true
		return nil
	}

	target, err := RestartNamedTarget(socketPath, configPath, "worker")
	if err != nil {
		t.Fatalf("restart named target: %v", err)
	}

	if target.Kind != LifecycleTargetWorkspace {
		t.Fatalf("target kind = %q, want workspace", target.Kind)
	}
	if got, want := strings.Join(steps, ","), "destroy:worker,create:worker"; got != want {
		t.Fatalf("steps = %q, want %q", got, want)
	}
}

func TestStartNamedTargetStartsManagedChildOrchestrator(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	socketPath := "/tmp/ax.sock"
	childDir := filepath.Join(home, "child")
	_ = writeDispatchConfig(t, childDir, "project: child\norchestrator_runtime: claude\nworkspaces:\n  dev:\n    dir: .\n    runtime: claude\n")
	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  root:\n    dir: .\n    runtime: claude\nchildren:\n  child:\n    dir: ./child\n    prefix: team\n")

	restoreWorkspaceSessionStubs(t)
	sessionExists := false
	created := false
	workspaceSessionExists = func(name string) bool {
		return name == "team.orchestrator" && sessionExists
	}
	workspaceCreateSessionWithArgs = func(name, dir string, argv []string) error {
		created = true
		sessionExists = true
		if name != "team.orchestrator" {
			return fmt.Errorf("unexpected orchestrator %q", name)
		}
		if dir != filepath.Join(childDir, ".ax", "orchestrator-team") {
			return fmt.Errorf("unexpected orchestrator dir %q", dir)
		}
		return nil
	}

	target, err := StartNamedTarget(socketPath, configPath, "team.orchestrator")
	if err != nil {
		t.Fatalf("start named target: %v", err)
	}

	if target.Kind != LifecycleTargetOrchestrator {
		t.Fatalf("target kind = %q, want orchestrator", target.Kind)
	}
	if !created {
		t.Fatal("expected managed orchestrator session to be created")
	}
}

func TestStopNamedTargetStopsManagedChildOrchestratorWithoutDeletingArtifacts(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	socketPath := "/tmp/ax.sock"
	childDir := filepath.Join(home, "child")
	_ = writeDispatchConfig(t, childDir, "project: child\norchestrator_runtime: claude\nworkspaces:\n  dev:\n    dir: .\n    runtime: claude\n")
	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  root:\n    dir: .\n    runtime: claude\nchildren:\n  child:\n    dir: ./child\n    prefix: team\n")

	tree, err := config.LoadTree(configPath)
	if err != nil {
		t.Fatalf("load tree: %v", err)
	}
	if len(tree.Children) != 1 {
		t.Fatalf("expected one child, got %d", len(tree.Children))
	}
	if err := EnsureOrchestrator(tree.Children[0], "orchestrator", socketPath, configPath, false); err != nil {
		t.Fatalf("ensure orchestrator artifacts: %v", err)
	}

	orchDir := filepath.Join(childDir, ".ax", "orchestrator-team")
	instructionFile, err := agent.InstructionFile(agent.RuntimeClaude)
	if err != nil {
		t.Fatalf("instruction file: %v", err)
	}

	restoreWorkspaceSessionStubs(t)
	sessionExists := true
	destroyCalled := false
	workspaceSessionExists = func(name string) bool {
		return name == "team.orchestrator" && sessionExists
	}
	workspaceDestroySession = func(name string) error {
		destroyCalled = true
		sessionExists = false
		return nil
	}

	target, err := StopNamedTarget(socketPath, configPath, "team.orchestrator")
	if err != nil {
		t.Fatalf("stop named target: %v", err)
	}

	if target.Kind != LifecycleTargetOrchestrator {
		t.Fatalf("target kind = %q, want orchestrator", target.Kind)
	}
	if !destroyCalled {
		t.Fatal("expected orchestrator session to be destroyed")
	}
	assertExists(t, filepath.Join(orchDir, ".mcp.json"))
	assertExists(t, filepath.Join(orchDir, instructionFile))
}

func TestStartNamedTargetRejectsRootOrchestrator(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configPath := writeDispatchConfig(t, home, "project: root\norchestrator_runtime: claude\nworkspaces:\n  root:\n    dir: .\n    runtime: claude\n")

	restoreWorkspaceSessionStubs(t)
	workspaceSessionExists = func(string) bool { return false }

	_, err := StartNamedTarget("/tmp/ax.sock", configPath, "orchestrator")
	if err == nil {
		t.Fatal("expected root orchestrator start to fail")
	}
	if !strings.Contains(err.Error(), `orchestrator "orchestrator" does not support targeted start because it is not a managed session`) {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestStartNamedTargetRejectsUnknownName(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n")

	restoreWorkspaceSessionStubs(t)
	workspaceSessionExists = func(string) bool { return false }

	_, err := StartNamedTarget("/tmp/ax.sock", configPath, "missing")
	if err == nil {
		t.Fatal("expected unknown target to fail")
	}
	if !strings.Contains(err.Error(), `target "missing" is not defined`) {
		t.Fatalf("unexpected error: %v", err)
	}
}
