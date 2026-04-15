package agent

import (
	"strings"
	"testing"
)

func TestCodexUserCommandAppendsPassthroughArgs(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	command, err := (codexRuntime{}).UserCommand("/tmp/workspace", "ws", "/tmp/ax.sock", "/tmp/ax", "", LaunchOptions{ExtraArgs: []string{"resume", "--last"}})
	if err != nil {
		t.Fatalf("UserCommand: %v", err)
	}
	if !strings.Contains(command, "CODEX_HOME=") {
		t.Fatalf("expected CODEX_HOME in user command, got %q", command)
	}
	if !strings.Contains(command, "'--dangerously-bypass-approvals-and-sandbox'") {
		t.Fatalf("expected codex launch flags in user command, got %q", command)
	}
	if !strings.Contains(command, "'resume'") || !strings.Contains(command, "'--last'") {
		t.Fatalf("expected passthrough args in user command, got %q", command)
	}
}
