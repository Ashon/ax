package cmd

import (
	"strings"
	"testing"
)

func TestResolveWorkspaceAttachTargetPrefersExactWorkspaceMatch(t *testing.T) {
	oldSessionExists := wsSessionExists
	t.Cleanup(func() {
		wsSessionExists = oldSessionExists
	})

	wsSessionExists = func(name string) bool {
		return name == "team" || name == "team.orchestrator"
	}

	if got := resolveWorkspaceAttachTarget("team"); got != "team" {
		t.Fatalf("attach target = %q, want %q", got, "team")
	}
}

func TestResolveWorkspaceAttachTargetFallsBackToProjectOrchestrator(t *testing.T) {
	oldSessionExists := wsSessionExists
	t.Cleanup(func() {
		wsSessionExists = oldSessionExists
	})

	wsSessionExists = func(name string) bool {
		return name == "team.orchestrator"
	}

	if got := resolveWorkspaceAttachTarget("team"); got != "team.orchestrator" {
		t.Fatalf("attach target = %q, want %q", got, "team.orchestrator")
	}
}

func TestWorkspaceAttachCommandReturnsHelpfulFallbackError(t *testing.T) {
	oldSessionExists := wsSessionExists
	oldAttachSession := wsAttachSession
	t.Cleanup(func() {
		wsSessionExists = oldSessionExists
		wsAttachSession = oldAttachSession
	})

	wsSessionExists = func(string) bool { return false }
	wsAttachSession = func(string) error {
		t.Fatal("attach should not run when no session exists")
		return nil
	}

	err := wsAttachCmd.RunE(wsAttachCmd, []string{"team"})
	if err == nil {
		t.Fatal("expected attach to fail when neither workspace nor orchestrator session exists")
	}
	if !strings.Contains(err.Error(), "team.orchestrator") {
		t.Fatalf("expected fallback orchestrator name in error, got %v", err)
	}
}
