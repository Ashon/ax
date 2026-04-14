package cmd

import (
	"bytes"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/config"
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
		printProjectTree(tree, 0, nil, nil)
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

	entries := buildSidebarFromTree(tree, nil)
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
