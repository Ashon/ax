package cmd

import (
	"strings"
	"testing"

	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
)

func TestTaskAttentionHintIncludesRecoveryGuidance(t *testing.T) {
	hint := taskAttentionHint(taskSummary{
		Diverged:       2,
		Stale:          1,
		QueuedMessages: 3,
	})

	for _, want := range []string{
		"2 diverged",
		"1 stale",
		"3 queued message(s)",
		"ax tasks --stale",
		"ax workspace list",
	} {
		if !strings.Contains(hint, want) {
			t.Fatalf("expected %q in hint %q", want, hint)
		}
	}
}

func TestTaskAttentionHintEmptyWithoutSignals(t *testing.T) {
	if got := taskAttentionHint(taskSummary{}); got != "" {
		t.Fatalf("expected empty hint, got %q", got)
	}
}

func TestWorkspaceStatusHelpers(t *testing.T) {
	workspaces := workspaceInfoMap([]types.WorkspaceInfo{
		{
			Name:       "ax.cli",
			Status:     types.StatusOnline,
			StatusText: "Inspecting operator UX and divergence visibility",
		},
	})

	if got := workspaceAgentStatus(workspaces, "ax.cli"); got != "online" {
		t.Fatalf("expected online agent status, got %q", got)
	}
	if got := workspaceAgentStatus(workspaces, "missing"); got != "offline" {
		t.Fatalf("expected offline status for missing workspace, got %q", got)
	}
	if got := workspaceStatusPreview(workspaces, "ax.cli", 18); !strings.HasPrefix(got, "Inspecting operato") {
		t.Fatalf("unexpected preview %q", got)
	}
	if got := workspaceStatusPreview(workspaces, "missing", 18); got != "" {
		t.Fatalf("expected empty preview for missing workspace, got %q", got)
	}
}

func TestBuildWorkspaceListRowsIncludesMatchedAndMismatchedStates(t *testing.T) {
	view := buildWorkspaceListRows(
		[]tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli", Attached: true},
			{Name: "ax-main", Workspace: "main", Attached: false},
		},
		workspaceInfoMap([]types.WorkspaceInfo{
			{Name: "ax.cli", Status: types.StatusOnline, StatusText: "Investigating CLI drift"},
			{Name: "ax.daemon", Status: types.StatusOnline, StatusText: "Running without tmux session"},
		}),
		map[string]string{
			"ax.cli":    "CLI owner",
			"ax.daemon": "Daemon owner",
			"main":      "Main workspace",
		},
		nil,
		false,
		true,
	)

	if len(view.HiddenInternal) != 0 {
		t.Fatalf("expected no hidden rows, got %+v", view.HiddenInternal)
	}
	if len(view.Rows) != 3 {
		t.Fatalf("expected 3 rows, got %+v", view.Rows)
	}

	expected := map[string]workspaceListRow{
		"ax.cli": {
			Name:        "ax.cli",
			Reconcile:   "",
			Tmux:        "attached",
			Agent:       "online",
			StatusText:  "Investigating CLI drift",
			Description: "CLI owner",
		},
		"ax.daemon": {
			Name:        "ax.daemon",
			Reconcile:   "",
			Tmux:        "no-session",
			Agent:       "online",
			StatusText:  "Running without tmux session",
			Description: "Daemon owner",
		},
		"main": {
			Name:        "main",
			Reconcile:   "",
			Tmux:        "detached",
			Agent:       "no-agent",
			StatusText:  "",
			Description: "Main workspace",
		},
	}

	for _, row := range view.Rows {
		want, ok := expected[row.Name]
		if !ok {
			t.Fatalf("unexpected row %+v", row)
		}
		if row != want {
			t.Fatalf("unexpected row for %s: got %+v want %+v", row.Name, row, want)
		}
	}
}

func TestBuildWorkspaceListRowsHidesInternalDaemonOnlyByDefault(t *testing.T) {
	view := buildWorkspaceListRows(
		nil,
		workspaceInfoMap([]types.WorkspaceInfo{
			{Name: "_cli", Status: types.StatusOnline},
			{Name: "ax.cli", Status: types.StatusOnline},
		}),
		nil,
		nil,
		false,
		false,
	)

	if len(view.Rows) != 1 || view.Rows[0].Name != "ax.cli" {
		t.Fatalf("expected only non-internal row, got %+v", view.Rows)
	}
	if len(view.HiddenInternal) != 1 || view.HiddenInternal[0] != "_cli" {
		t.Fatalf("expected hidden _cli row, got %+v", view.HiddenInternal)
	}
}

func TestBuildWorkspaceListRowsIncludesAndLabelsInternalWhenRequested(t *testing.T) {
	view := buildWorkspaceListRows(
		nil,
		workspaceInfoMap([]types.WorkspaceInfo{
			{Name: "_cli", Status: types.StatusOnline},
		}),
		nil,
		nil,
		false,
		true,
	)

	if len(view.HiddenInternal) != 0 {
		t.Fatalf("expected no hidden rows, got %+v", view.HiddenInternal)
	}
	if len(view.Rows) != 1 {
		t.Fatalf("expected one visible row, got %+v", view.Rows)
	}
	row := view.Rows[0]
	if row.Name != "_cli" || row.Tmux != "no-session" || row.Agent != "online" || row.Description != "internal daemon identity" {
		t.Fatalf("unexpected internal row %+v", row)
	}
}

func TestBuildWorkspaceListRowsShowsReconfigureStateWhenEnabled(t *testing.T) {
	view := buildWorkspaceListRows(
		[]tmux.SessionInfo{
			{Name: "ax-old", Workspace: "old", Attached: false},
		},
		nil,
		map[string]string{
			"main": "Main workspace",
		},
		map[string]bool{
			"main": true,
		},
		true,
		true,
	)

	if !view.ReconfigureEnabled {
		t.Fatal("expected reconfigure view to be enabled")
	}
	if len(view.Rows) != 2 {
		t.Fatalf("expected two rows, got %+v", view.Rows)
	}

	expected := map[string]string{
		"main": "desired-only",
		"old":  "runtime-only",
	}
	for _, row := range view.Rows {
		if got, ok := expected[row.Name]; !ok || row.Reconcile != got {
			t.Fatalf("unexpected row %+v", row)
		}
	}
}

func TestFormatHiddenInternalWorkspaceNote(t *testing.T) {
	note := formatHiddenInternalWorkspaceNote([]string{"_cli", "_foo"})
	for _, want := range []string{
		"Hidden 2 internal daemon-only workspaces",
		"_cli, _foo",
		"--internal",
	} {
		if !strings.Contains(note, want) {
			t.Fatalf("expected %q in note %q", want, note)
		}
	}
}
