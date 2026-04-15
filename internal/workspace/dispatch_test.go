package workspace

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestDispatchRunnableWorkCreatesMissingWorkspaceSessionAndWakes(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n")

	restoreWorkspaceSessionStubs(t)
	sessionExists := false
	created := false
	woke := false

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
	workspaceSessionIdle = func(name string) bool {
		return name == "worker" && sessionExists
	}
	workspaceWakeSession = func(target, prompt string) error {
		woke = true
		if !sessionExists {
			return fmt.Errorf("wake called before create")
		}
		if target != "worker" {
			return fmt.Errorf("unexpected wake target %q", target)
		}
		if !strings.Contains(prompt, `send_message(to="ax.orchestrator")`) {
			return fmt.Errorf("wake prompt missing sender: %q", prompt)
		}
		return nil
	}

	if err := DispatchRunnableWork("/tmp/ax.sock", configPath, "worker", "ax.orchestrator", false); err != nil {
		t.Fatalf("dispatch runnable work: %v", err)
	}

	if !created {
		t.Fatal("expected missing workspace session to be created")
	}
	if !woke {
		t.Fatal("expected workspace to be woken")
	}
}

func TestDispatchRunnableWorkFreshWorkspaceRestartsBeforeWake(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

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
	workspaceSessionIdle = func(name string) bool {
		return name == "worker" && sessionExists
	}
	workspaceWakeSession = func(target, prompt string) error {
		steps = append(steps, "wake:"+target)
		if !strings.Contains(prompt, "fresh-context") {
			return fmt.Errorf("fresh wake prompt missing marker: %q", prompt)
		}
		return nil
	}

	if err := DispatchRunnableWork("/tmp/ax.sock", configPath, "worker", "ax.orchestrator", true); err != nil {
		t.Fatalf("dispatch runnable work: %v", err)
	}

	if got, want := strings.Join(steps, ","), "destroy:worker,create:worker,wake:worker"; got != want {
		t.Fatalf("steps = %q, want %q", got, want)
	}
}

func TestDispatchRunnableWorkStartsMissingManagedOrchestrator(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	childDir := filepath.Join(home, "child")
	_ = writeDispatchConfig(t, childDir, "project: child\norchestrator_runtime: claude\nworkspaces:\n  dev:\n    dir: .\n    runtime: claude\n")
	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  root:\n    dir: .\n    runtime: claude\nchildren:\n  child:\n    dir: ./child\n    prefix: team\n")

	restoreWorkspaceSessionStubs(t)
	sessionExists := false
	created := false
	woke := false

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
	workspaceSessionIdle = func(name string) bool {
		return name == "team.orchestrator" && sessionExists
	}
	workspaceWakeSession = func(target, prompt string) error {
		woke = true
		if !sessionExists {
			return fmt.Errorf("wake called before orchestrator start")
		}
		if target != "team.orchestrator" {
			return fmt.Errorf("unexpected wake target %q", target)
		}
		return nil
	}

	if err := DispatchRunnableWork("/tmp/ax.sock", configPath, "team.orchestrator", "ax.orchestrator", false); err != nil {
		t.Fatalf("dispatch runnable work: %v", err)
	}

	if !created {
		t.Fatal("expected managed orchestrator session to be created")
	}
	if !woke {
		t.Fatal("expected managed orchestrator to be woken")
	}
}

func TestDispatchRunnableWorkWaitsForNewSessionPromptBeforeWake(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n")

	restoreWorkspaceSessionStubs(t)
	sessionExists := false
	idleChecks := 0
	wakeIdleChecks := 0
	dispatchTargetReadyTimeout = 5 * time.Millisecond
	dispatchTargetReadyPollInterval = 1 * time.Millisecond
	dispatchTargetReadySettleDelay = 0
	dispatchTargetReadyFallbackDelay = 0

	workspaceSessionExists = func(name string) bool {
		return name == "worker" && sessionExists
	}
	workspaceCreateSessionWithArgsEnv = func(name, dir string, argv []string, env map[string]string) error {
		sessionExists = true
		return nil
	}
	workspaceSessionIdle = func(name string) bool {
		if name != "worker" || !sessionExists {
			return false
		}
		idleChecks++
		return idleChecks >= 3
	}
	workspaceWakeSession = func(target, prompt string) error {
		wakeIdleChecks = idleChecks
		return nil
	}

	if err := DispatchRunnableWork("/tmp/ax.sock", configPath, "worker", "ax.orchestrator", false); err != nil {
		t.Fatalf("dispatch runnable work: %v", err)
	}

	if wakeIdleChecks < 3 {
		t.Fatalf("wake ran before startup prompt was observed: idle_checks=%d", wakeIdleChecks)
	}
}

func TestDispatchRunnableWorkDoesNotWaitForExistingBusySession(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configPath := writeDispatchConfig(t, home, "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n")

	restoreWorkspaceSessionStubs(t)
	idleChecks := 0

	workspaceSessionExists = func(name string) bool {
		return name == "worker"
	}
	workspaceSessionIdle = func(name string) bool {
		idleChecks++
		return false
	}
	workspaceWakeSession = func(target, prompt string) error {
		return nil
	}

	if err := DispatchRunnableWork("/tmp/ax.sock", configPath, "worker", "ax.orchestrator", false); err != nil {
		t.Fatalf("dispatch runnable work: %v", err)
	}

	if idleChecks != 0 {
		t.Fatalf("expected existing session dispatch to skip startup wait, got %d idle checks", idleChecks)
	}
}

func TestEnsureDispatchTargetRejectsMissingRootOrchestratorSession(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configPath := writeDispatchConfig(t, home, "project: root\norchestrator_runtime: claude\nworkspaces:\n  root:\n    dir: .\n    runtime: claude\n")

	restoreWorkspaceSessionStubs(t)
	workspaceSessionExists = func(string) bool { return false }

	err := EnsureDispatchTarget("/tmp/ax.sock", configPath, "orchestrator", false)
	if err == nil {
		t.Fatal("expected missing unmanaged root orchestrator to fail")
	}
	if !strings.Contains(err.Error(), "is not running and is not a managed session") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func writeDispatchConfig(t *testing.T, dir, content string) string {
	t.Helper()

	configPath := filepath.Join(dir, ".ax", "config.yaml")
	writeTestFile(t, configPath, content)
	return configPath
}
