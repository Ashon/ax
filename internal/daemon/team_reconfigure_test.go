package daemon

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/types"
)

func TestBeginTeamReconfigureApplySerializesAndRecordsReport(t *testing.T) {
	stateDir := t.TempDir()
	cfgPath := writeTeamConfig(t, stateDir, `
project: demo
experimental_mcp_team_reconfigure: true
workspaces:
  main:
    dir: .
`)

	d := New(filepath.Join(stateDir, "daemon.sock"))
	d.sharedValues[types.ExperimentalMCPTeamReconfigureFlagKey] = "true"

	changes := []types.TeamReconfigureChange{
		{
			Op:   types.TeamChangeAdd,
			Kind: types.TeamEntryWorkspace,
			Name: "helper",
			Workspace: &types.TeamWorkspaceSpec{
				Dir:         "./helper",
				Description: "helper workspace",
				Runtime:     "claude",
			},
		},
	}

	ticket, err := d.beginTeamReconfigureApply(cfgPath, nil, changes, types.TeamReconcileArtifactsOnly)
	if err != nil {
		t.Fatalf("begin apply: %v", err)
	}
	if ticket.Plan.State.Revision != 1 {
		t.Fatalf("expected revision 1 after begin apply, got %d", ticket.Plan.State.Revision)
	}
	if ticket.ReconcileMode != types.TeamReconcileArtifactsOnly {
		t.Fatalf("expected reconcile mode to round-trip, got %q", ticket.ReconcileMode)
	}

	if _, err := d.beginTeamReconfigureApply(cfgPath, nil, []types.TeamReconfigureChange{
		{
			Op:   types.TeamChangeRemove,
			Kind: types.TeamEntryWorkspace,
			Name: "helper",
		},
	}, types.TeamReconcileArtifactsOnly); err == nil || !strings.Contains(err.Error(), "already in progress") {
		t.Fatalf("expected second apply to be serialized, got err=%v", err)
	}

	state, err := d.finishTeamReconfigureApply(ticket.Token, true, "", []types.TeamReconfigureAction{
		{
			Action: "create",
			Kind:   types.TeamEntryWorkspace,
			Name:   "helper",
		},
	})
	if err != nil {
		t.Fatalf("finish apply: %v", err)
	}
	if state.LastApply == nil {
		t.Fatal("expected last_apply report to be stored")
	}
	if !state.LastApply.Success {
		t.Fatalf("expected successful last_apply report, got %+v", state.LastApply)
	}
	if state.LastApply.ReconcileMode != types.TeamReconcileArtifactsOnly {
		t.Fatalf("expected reconcile mode in report, got %q", state.LastApply.ReconcileMode)
	}
	if got := len(state.LastApply.Actions); got != 1 {
		t.Fatalf("expected one recorded action, got %d", got)
	}

	if _, err := d.beginTeamReconfigureApply(cfgPath, intPtr(1), []types.TeamReconfigureChange{
		{
			Op:   types.TeamChangeRemove,
			Kind: types.TeamEntryWorkspace,
			Name: "helper",
		},
	}, types.TeamReconcileArtifactsOnly); err != nil {
		t.Fatalf("expected apply lease to be released after finish, got %v", err)
	}
}

func TestDryRunTeamReconfigureRejectsRevisionMismatch(t *testing.T) {
	stateDir := t.TempDir()
	cfgPath := writeTeamConfig(t, stateDir, `
project: demo
experimental_mcp_team_reconfigure: true
workspaces:
  main:
    dir: .
`)

	d := New(filepath.Join(stateDir, "daemon.sock"))
	d.sharedValues[types.ExperimentalMCPTeamReconfigureFlagKey] = "true"

	changes := []types.TeamReconfigureChange{
		{
			Op:   types.TeamChangeAdd,
			Kind: types.TeamEntryWorkspace,
			Name: "helper",
			Workspace: &types.TeamWorkspaceSpec{
				Dir: "./helper",
			},
		},
	}

	ticket, err := d.beginTeamReconfigureApply(cfgPath, nil, changes, types.TeamReconcileArtifactsOnly)
	if err != nil {
		t.Fatalf("begin apply: %v", err)
	}
	if _, err := d.finishTeamReconfigureApply(ticket.Token, true, "", nil); err != nil {
		t.Fatalf("finish apply: %v", err)
	}

	if _, err := d.dryRunTeamReconfigure(cfgPath, intPtr(0), []types.TeamReconfigureChange{
		{
			Op:   types.TeamChangeRemove,
			Kind: types.TeamEntryWorkspace,
			Name: "helper",
		},
	}); err == nil || !strings.Contains(err.Error(), "revision mismatch") {
		t.Fatalf("expected revision mismatch error, got %v", err)
	}
}

func writeTeamConfig(t *testing.T, rootDir, content string) string {
	t.Helper()
	cfgPath := filepath.Join(rootDir, ".ax", "config.yaml")
	if err := os.MkdirAll(filepath.Dir(cfgPath), 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(cfgPath, []byte(strings.TrimSpace(content)+"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	return cfgPath
}

func intPtr(value int) *int {
	return &value
}
