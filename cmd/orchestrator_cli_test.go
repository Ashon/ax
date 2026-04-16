package cmd

import (
	"reflect"
	"strings"
	"testing"
)

func TestWrapRootOrchestratorEphemeralArgvPreservesOriginalCommand(t *testing.T) {
	argv := []string{"ax", "run-agent", "--runtime", "codex", "--workspace", "orchestrator"}

	got := wrapRootOrchestratorEphemeralArgv(argv)
	if len(got) != len(argv)+4 {
		t.Fatalf("wrapped argv length = %d, want %d (%v)", len(got), len(argv)+4, got)
	}
	if got[0] != "sh" || got[1] != "-lc" {
		t.Fatalf("expected shell wrapper prefix, got %v", got[:2])
	}
	if !strings.Contains(got[2], "Root orchestrator process exited unexpectedly") {
		t.Fatalf("expected failure-hold script in argv, got %q", got[2])
	}
	if got[3] != "ax-root-orchestrator" {
		t.Fatalf("wrapper argv[3] = %q, want ax-root-orchestrator", got[3])
	}
	if !reflect.DeepEqual(got[4:], argv) {
		t.Fatalf("wrapped tail = %v, want %v", got[4:], argv)
	}
}

func TestWrapRootOrchestratorEphemeralArgvHandlesEmptyInput(t *testing.T) {
	if got := wrapRootOrchestratorEphemeralArgv(nil); got != nil {
		t.Fatalf("expected nil for empty argv, got %v", got)
	}
}
