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

	cmd := newClaudeCommand(dir, true, nil)

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
	cmd := newClaudeCommand(t.TempDir(), false, nil)

	if slices.Contains(cmd.Args, "--continue") {
		t.Fatalf("expected fallback command to omit --continue, got %v", cmd.Args)
	}
	if !slices.Contains(cmd.Env, claudePromptSuggestionDisabledEnv) {
		t.Fatalf("expected %q in command env, got %v", claudePromptSuggestionDisabledEnv, cmd.Env)
	}
}

func TestNewClaudeCommandAppendsPassthroughArgsWithoutImplicitContinue(t *testing.T) {
	cmd := newClaudeCommand(t.TempDir(), false, []string{"--resume", "session-123"})

	if slices.Contains(cmd.Args, "--continue") {
		t.Fatalf("expected passthrough command to omit implicit --continue, got %v", cmd.Args)
	}
	if !slices.Contains(cmd.Args, "--resume") || !slices.Contains(cmd.Args, "session-123") {
		t.Fatalf("expected passthrough args in command, got %v", cmd.Args)
	}
}

func TestClaudeUserCommandDisablesPromptSuggestions(t *testing.T) {
	dir := t.TempDir()
	instructions := "Stay aligned with orchestrator instructions."
	if err := os.WriteFile(filepath.Join(dir, "CLAUDE.md"), []byte(instructions), 0o644); err != nil {
		t.Fatalf("write CLAUDE.md: %v", err)
	}

	command, err := (claudeRuntime{}).UserCommand(dir, "ws", "", "", "", LaunchOptions{})
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

func TestClaudeUserCommandPassthroughArgsSkipContinueFallback(t *testing.T) {
	dir := t.TempDir()
	instructions := "Stay aligned with orchestrator instructions."
	if err := os.WriteFile(filepath.Join(dir, "CLAUDE.md"), []byte(instructions), 0o644); err != nil {
		t.Fatalf("write CLAUDE.md: %v", err)
	}

	command, err := (claudeRuntime{}).UserCommand(dir, "ws", "", "", "", LaunchOptions{ExtraArgs: []string{"--resume", "session-123"}})
	if err != nil {
		t.Fatalf("UserCommand: %v", err)
	}
	if strings.Contains(command, "||") {
		t.Fatalf("expected explicit passthrough command without fallback, got %q", command)
	}
	if strings.Contains(command, "--continue") {
		t.Fatalf("expected explicit passthrough command to avoid implicit continue, got %q", command)
	}
	if !strings.Contains(command, "--resume") || !strings.Contains(command, "session-123") {
		t.Fatalf("expected passthrough args in user command, got %q", command)
	}
}

func TestPrepareClaudeLaunchFreshRemovesProjectState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	dir := filepath.Join(home, "repo")
	projectPath, err := claudeProjectPath(dir)
	if err != nil {
		t.Fatalf("claudeProjectPath: %v", err)
	}
	if err := os.MkdirAll(projectPath, 0o755); err != nil {
		t.Fatalf("mkdir project path: %v", err)
	}
	if err := os.WriteFile(filepath.Join(projectPath, "transcript.jsonl"), []byte("stale"), 0o644); err != nil {
		t.Fatalf("write transcript: %v", err)
	}

	if err := prepareClaudeLaunch(dir, true); err != nil {
		t.Fatalf("prepareClaudeLaunch: %v", err)
	}
	if _, err := os.Stat(projectPath); !os.IsNotExist(err) {
		t.Fatalf("expected project path %q to be removed, stat err=%v", projectPath, err)
	}
}

func TestClaudeUserCommandFreshStartSkipsContinueFallback(t *testing.T) {
	command, err := (claudeRuntime{}).UserCommand(t.TempDir(), "ws", "", "", "", LaunchOptions{FreshStart: true})
	if err != nil {
		t.Fatalf("UserCommand: %v", err)
	}
	if strings.Contains(command, "||") {
		t.Fatalf("expected fresh start command without fallback, got %q", command)
	}
	if strings.Contains(command, "--continue") {
		t.Fatalf("expected fresh start command to omit --continue, got %q", command)
	}
}
