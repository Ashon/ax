package workspace

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
)

func TestReconcileDesiredStateCreatesAndCleansGeneratedArtifacts(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configPath := filepath.Join(home, "project", ".ax", "config.yaml")
	socketPath := "/tmp/ax.sock"
	reconciler := NewReconciler(socketPath, configPath)

	oldListSessions := listTmuxSessions
	oldSessionIdle := tmuxSessionIdle
	t.Cleanup(func() {
		listTmuxSessions = oldListSessions
		tmuxSessionIdle = oldSessionIdle
	})
	listTmuxSessions = func() ([]tmux.SessionInfo, error) { return nil, nil }
	tmuxSessionIdle = func(string) bool { return true }

	oldWorkspaceDir := filepath.Join(home, "project", "old")
	if err := EnsureArtifacts("old", config.Workspace{
		Dir:          oldWorkspaceDir,
		Runtime:      agent.RuntimeCodex,
		Instructions: "old workspace instructions",
	}, socketPath, configPath); err != nil {
		t.Fatalf("ensure old workspace artifacts: %v", err)
	}
	oldWorkspaceCodexHome, err := agent.CodexHomePath("old", oldWorkspaceDir)
	if err != nil {
		t.Fatalf("workspace codex home: %v", err)
	}

	oldOrchestratorDir := filepath.Join(home, "project", ".ax", "orchestrator-team")
	oldNode := &config.ProjectNode{
		Name:                "team",
		Prefix:              "team",
		Dir:                 filepath.Join(home, "project"),
		OrchestratorRuntime: agent.RuntimeCodex,
	}
	if err := EnsureOrchestrator(oldNode, "orchestrator", socketPath, configPath, false); err != nil {
		t.Fatalf("ensure old orchestrator artifacts: %v", err)
	}
	oldOrchestratorCodexHome, err := agent.CodexHomePath("team.orchestrator", oldOrchestratorDir)
	if err != nil {
		t.Fatalf("orchestrator codex home: %v", err)
	}

	previous := newReconcileState()
	previous.SocketPath = daemon.ExpandSocketPath(socketPath)
	previous.ConfigPath = cleanPath(configPath)
	previous.Workspaces["old"] = workspaceState{
		Name:             "old",
		Dir:              cleanPath(oldWorkspaceDir),
		Runtime:          agent.RuntimeCodex,
		InstructionsHash: hashText("old"),
	}
	previous.Orchestrators["team.orchestrator"] = orchestratorState{
		Name:           "team.orchestrator",
		ArtifactDir:    cleanPath(oldOrchestratorDir),
		Runtime:        agent.RuntimeCodex,
		ParentName:     "orchestrator",
		PromptHash:     hashText("old"),
		ManagedSession: true,
	}
	if err := saveReconcileState(reconcileStatePath(cleanPath(configPath)), previous); err != nil {
		t.Fatalf("save previous state: %v", err)
	}

	newWorkspaceDir := filepath.Join(home, "project", "new")
	desired := &DesiredState{
		SocketPath: daemon.ExpandSocketPath(socketPath),
		ConfigPath: cleanPath(configPath),
		Workspaces: map[string]DesiredWorkspace{
			"new": {
				Name: "new",
				Workspace: config.Workspace{
					Dir:          newWorkspaceDir,
					Runtime:      agent.RuntimeClaude,
					Instructions: "new workspace instructions",
				},
			},
		},
		Orchestrators: map[string]DesiredOrchestrator{},
	}

	report, err := reconciler.ReconcileDesiredState(desired, ReconcileOptions{DaemonRunning: false})
	if err != nil {
		t.Fatalf("reconcile: %v", err)
	}
	if len(report.Actions) < 2 {
		t.Fatalf("expected create/remove actions, got %+v", report.Actions)
	}

	assertNotExists(t, filepath.Join(oldWorkspaceDir, ".mcp.json"))
	assertNotExists(t, filepath.Join(oldWorkspaceDir, "AGENTS.md"))
	assertNotExists(t, oldWorkspaceCodexHome)
	assertNotExists(t, oldOrchestratorDir)
	assertNotExists(t, oldOrchestratorCodexHome)

	assertExists(t, filepath.Join(newWorkspaceDir, ".mcp.json"))
	assertExists(t, filepath.Join(newWorkspaceDir, "CLAUDE.md"))

	saved, err := loadReconcileState(reconcileStatePath(cleanPath(configPath)))
	if err != nil {
		t.Fatalf("load saved state: %v", err)
	}
	if len(saved.Workspaces) != 1 {
		t.Fatalf("expected one workspace in saved state, got %+v", saved.Workspaces)
	}
	if saved.Workspaces["new"].Dir != cleanPath(newWorkspaceDir) {
		t.Fatalf("expected new workspace dir %q, got %+v", cleanPath(newWorkspaceDir), saved.Workspaces["new"])
	}
	if len(saved.Orchestrators) != 0 {
		t.Fatalf("expected orchestrators removed from saved state, got %+v", saved.Orchestrators)
	}
}

func TestReconcileDesiredStateBlocksBusyWorkspaceRestart(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configPath := filepath.Join(home, "project", ".ax", "config.yaml")
	socketPath := "/tmp/ax.sock"
	reconciler := NewReconciler(socketPath, configPath)

	previous := newReconcileState()
	previous.SocketPath = daemon.ExpandSocketPath(socketPath)
	previous.ConfigPath = cleanPath(configPath)
	previous.Workspaces["alpha"] = workspaceState{
		Name:             "alpha",
		Dir:              cleanPath(filepath.Join(home, "project", "alpha")),
		Runtime:          agent.RuntimeClaude,
		InstructionsHash: hashText("old instructions"),
	}
	if err := saveReconcileState(reconcileStatePath(cleanPath(configPath)), previous); err != nil {
		t.Fatalf("save previous state: %v", err)
	}

	oldListSessions := listTmuxSessions
	oldSessionIdle := tmuxSessionIdle
	t.Cleanup(func() {
		listTmuxSessions = oldListSessions
		tmuxSessionIdle = oldSessionIdle
	})

	listTmuxSessions = func() ([]tmux.SessionInfo, error) {
		return []tmux.SessionInfo{
			{
				Name:      tmux.SessionName("alpha"),
				Workspace: "alpha",
				Attached:  false,
			},
		}, nil
	}
	tmuxSessionIdle = func(string) bool { return false }

	desired := &DesiredState{
		SocketPath: daemon.ExpandSocketPath(socketPath),
		ConfigPath: cleanPath(configPath),
		Workspaces: map[string]DesiredWorkspace{
			"alpha": {
				Name: "alpha",
				Workspace: config.Workspace{
					Dir:          filepath.Join(home, "project", "alpha"),
					Runtime:      agent.RuntimeClaude,
					Instructions: "new instructions",
				},
			},
		},
		Orchestrators: map[string]DesiredOrchestrator{},
	}

	report, err := reconciler.ReconcileDesiredState(desired, ReconcileOptions{DaemonRunning: true})
	if err != nil {
		t.Fatalf("reconcile: %v", err)
	}

	foundBlocked := false
	for _, action := range report.Actions {
		if action.Kind == "workspace" && action.Name == "alpha" && action.Operation == "blocked_restart" {
			foundBlocked = true
			break
		}
	}
	if !foundBlocked {
		t.Fatalf("expected blocked_restart action, got %+v", report.Actions)
	}

	saved, err := loadReconcileState(reconcileStatePath(cleanPath(configPath)))
	if err != nil {
		t.Fatalf("load saved state: %v", err)
	}
	if got := saved.Workspaces["alpha"].InstructionsHash; got != previous.Workspaces["alpha"].InstructionsHash {
		t.Fatalf("expected blocked restart to keep previous state hash %q, got %q", previous.Workspaces["alpha"].InstructionsHash, got)
	}
}

func writeTestFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir %s: %v", filepath.Dir(path), err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func assertExists(t *testing.T, path string) {
	t.Helper()
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("expected %s to exist: %v", path, err)
	}
}

func assertNotExists(t *testing.T, path string) {
	t.Helper()
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatalf("expected %s to be removed, stat err=%v", path, err)
	}
}
