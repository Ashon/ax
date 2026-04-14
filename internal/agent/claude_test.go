package agent

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

func TestNewClaudeCommandDisablesPromptSuggestionsAndContinues(t *testing.T) {
	dir := t.TempDir()
	instructions := "Use ax workspace instructions."
	if err := os.WriteFile(filepath.Join(dir, "CLAUDE.md"), []byte(instructions), 0o644); err != nil {
		t.Fatalf("write CLAUDE.md: %v", err)
	}

	cmd := newClaudeCommand(dir, true)

	if cmd.Dir != dir {
		t.Fatalf("expected command dir %q, got %q", dir, cmd.Dir)
	}
	if !slices.Contains(cmd.Env, claudePromptSuggestionDisabledEnv) {
		t.Fatalf("expected %q in command env, got %v", claudePromptSuggestionDisabledEnv, cmd.Env)
	}
	if !slices.Contains(cmd.Args, "--continue") {
		t.Fatalf("expected --continue in args, got %v", cmd.Args)
	}
	if !slices.Contains(cmd.Args, "--append-system-prompt") {
		t.Fatalf("expected --append-system-prompt in args, got %v", cmd.Args)
	}
	if !slices.Contains(cmd.Args, instructions) {
		t.Fatalf("expected CLAUDE.md contents in args, got %v", cmd.Args)
	}
}

func TestNewClaudeCommandOmitsContinueForFallback(t *testing.T) {
	cmd := newClaudeCommand(t.TempDir(), false)

	if slices.Contains(cmd.Args, "--continue") {
		t.Fatalf("expected fallback command to omit --continue, got %v", cmd.Args)
	}
	if !slices.Contains(cmd.Env, claudePromptSuggestionDisabledEnv) {
		t.Fatalf("expected %q in command env, got %v", claudePromptSuggestionDisabledEnv, cmd.Env)
	}
}

func TestClaudeUserCommandDisablesPromptSuggestions(t *testing.T) {
	dir := t.TempDir()
	instructions := "Stay aligned with orchestrator instructions."
	if err := os.WriteFile(filepath.Join(dir, "CLAUDE.md"), []byte(instructions), 0o644); err != nil {
		t.Fatalf("write CLAUDE.md: %v", err)
	}

	command, err := (claudeRuntime{}).UserCommand(dir, "ws", "", "", "")
	if err != nil {
		t.Fatalf("UserCommand: %v", err)
	}
	if !strings.Contains(command, claudePromptSuggestionDisabledEnv+" claude") {
		t.Fatalf("expected disable env in user command, got %q", command)
	}
	if !strings.Contains(command, "--dangerously-skip-permissions") {
		t.Fatalf("expected launch flags in user command, got %q", command)
	}
	if !strings.Contains(command, "--append-system-prompt") {
		t.Fatalf("expected append-system-prompt in user command, got %q", command)
	}
	if !strings.Contains(command, instructions) {
		t.Fatalf("expected CLAUDE.md contents in user command, got %q", command)
	}
	if !strings.Contains(command, "--continue") || !strings.Contains(command, "||") {
		t.Fatalf("expected continue fallback shell command, got %q", command)
	}
}
