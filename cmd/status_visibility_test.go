package cmd

import (
	"bytes"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/tmux"
)

func TestCollectKnownWorkspacesSkipsDisabledRootOrchestrator(t *testing.T) {
	tree := &config.ProjectNode{
		Name:                    "root",
		DisableRootOrchestrator: true,
		Workspaces: []config.WorkspaceRef{
			{Name: "main", MergedName: "main"},
		},
		Children: []*config.ProjectNode{
			{
				Name:   "child",
				Prefix: "team",
				Workspaces: []config.WorkspaceRef{
					{Name: "worker", MergedName: "team.worker"},
				},
			},
		},
	}

	known := map[string]bool{}
	collectKnownWorkspaces(tree, known)

	if known["orchestrator"] {
		t.Fatalf("expected disabled root orchestrator to be omitted from known workspaces: %+v", known)
	}
	for _, want := range []string{"main", "team.worker", "team.orchestrator"} {
		if !known[want] {
			t.Fatalf("expected %q in known workspaces: %+v", want, known)
		}
	}
}

func TestPrintProjectTreeSkipsDisabledRootOrchestratorButKeepsChildren(t *testing.T) {
	tree := &config.ProjectNode{
		Name:                    "root",
		DisableRootOrchestrator: true,
		Workspaces: []config.WorkspaceRef{
			{Name: "main", MergedName: "main"},
		},
		Children: []*config.ProjectNode{
			{
				Name:   "child",
				Prefix: "team",
			},
		},
	}

	output := captureStdout(t, func() {
		printProjectTree(tree, 0, nil, nil, false)
	})

	if strings.Contains(output, "\n  ○ ◆ orchestrator") {
		t.Fatalf("did not expect root orchestrator line in output %q", output)
	}
	if !strings.Contains(output, "\n    ○ ◆ orchestrator") {
		t.Fatalf("expected child orchestrator line in output %q", output)
	}
}

func TestBuildSidebarFromTreeSkipsDisabledRootOrchestrator(t *testing.T) {
	tree := &config.ProjectNode{
		Name:                    "root",
		DisableRootOrchestrator: true,
		Workspaces: []config.WorkspaceRef{
			{Name: "main", MergedName: "main"},
		},
		Children: []*config.ProjectNode{
			{
				Name:   "child",
				Prefix: "team",
			},
		},
	}

	entries := buildSidebarFromTree(tree, nil, false, nil)
	var orchestratorEntries []sidebarEntry
	for _, entry := range entries {
		if entry.label == "◆ orchestrator" {
			orchestratorEntries = append(orchestratorEntries, entry)
		}
	}

	if len(orchestratorEntries) != 1 {
		t.Fatalf("expected exactly one visible orchestrator entry, got %+v", orchestratorEntries)
	}
	if orchestratorEntries[0].level != 2 {
		t.Fatalf("expected child orchestrator entry at level 2, got %+v", orchestratorEntries[0])
	}
}

func TestPrintProjectTreeShowsDesiredStateWhenReconfigureEnabled(t *testing.T) {
	tree := &config.ProjectNode{
		Name: "root",
		Workspaces: []config.WorkspaceRef{
			{Name: "main", MergedName: "main"},
		},
	}

	output := captureStdout(t, func() {
		printProjectTree(tree, 0, nil, nil, true)
	})

	if !strings.Contains(output, "desired") {
		t.Fatalf("expected desired state in output %q", output)
	}
}

func TestBuildSidebarFromTreeLabelsRuntimeOnlyGroupWhenReconfigureEnabled(t *testing.T) {
	tree := &config.ProjectNode{
		Name: "root",
		Workspaces: []config.WorkspaceRef{
			{Name: "main", MergedName: "main"},
		},
	}

	entries := buildSidebarFromTree(tree, []tmux.SessionInfo{
		{Name: "ax-old", Workspace: "old"},
	}, true, map[string]bool{"main": true})

	if len(entries) < 4 {
		t.Fatalf("expected tree + workspace + runtime-only group, got %+v", entries)
	}
	var foundGroup, foundDesired, foundRuntimeOnly bool
	for _, entry := range entries {
		if entry.group && entry.label == "▾ runtime-only (not in config tree)" {
			foundGroup = true
		}
		if entry.workspace == "main" && entry.reconcile == "desired" {
			foundDesired = true
		}
		if entry.workspace == "old" && entry.reconcile == "runtime-only" {
			foundRuntimeOnly = true
		}
	}
	if !foundGroup || !foundDesired || !foundRuntimeOnly {
		t.Fatalf("expected runtime-only labeling in entries %+v", entries)
	}
}

func TestLoadWatchRuntimesOmitsDisabledRootOrchestrator(t *testing.T) {
	rootDir := t.TempDir()
	cfgPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeTestConfig(t, cfgPath, `
project: root
disable_root_orchestrator: true
workspaces:
  main:
    dir: .
    runtime: codex
`)

	oldConfigPath := configPath
	t.Cleanup(func() { configPath = oldConfigPath })
	configPath = cfgPath

	runtimes := loadWatchRuntimes()

	if _, ok := runtimes["orchestrator"]; ok {
		t.Fatalf("expected disabled root orchestrator runtime to be omitted, got %+v", runtimes)
	}
	if runtimes["main"] != "codex" {
		t.Fatalf("expected main runtime to remain available, got %+v", runtimes)
	}
}

func captureStdout(t *testing.T, fn func()) string {
	t.Helper()

	oldStdout := os.Stdout
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("pipe: %v", err)
	}
	os.Stdout = w
	defer func() { os.Stdout = oldStdout }()

	fn()

	if err := w.Close(); err != nil {
		t.Fatalf("close writer: %v", err)
	}
	var buf bytes.Buffer
	if _, err := io.Copy(&buf, r); err != nil {
		t.Fatalf("read stdout: %v", err)
	}
	if err := r.Close(); err != nil {
		t.Fatalf("close reader: %v", err)
	}
	return buf.String()
}
