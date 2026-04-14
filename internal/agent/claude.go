package agent

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

type claudeRuntime struct{}

const claudePromptSuggestionDisabledEnv = "CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=false"

func (claudeRuntime) Name() string {
	return RuntimeClaude
}

func (claudeRuntime) InstructionFile() string {
	return "CLAUDE.md"
}

func (claudeRuntime) Launch(dir, workspace, socketPath, axBin, configPath string) error {
	cmd := newClaudeCommand(dir, true)
	if err := cmd.Run(); err == nil {
		return nil
	}

	// Fallback without --continue
	fallback := newClaudeCommand(dir, false)
	return fallback.Run()
}

func newClaudeCommand(dir string, continueSession bool) *exec.Cmd {
	args := claudeCommandArgs(dir, continueSession)
	cmd := exec.Command("claude", args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(), claudePromptSuggestionDisabledEnv)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd
}

func loadInstructionsFile(dir, name string) string {
	data, err := os.ReadFile(filepath.Join(dir, name))
	if err != nil {
		return ""
	}
	return string(data)
}

func (claudeRuntime) UserCommand(dir, workspace, socketPath, axBin, configPath string) (string, error) {
	primary := claudeCommandString(dir, true)
	fallback := claudeCommandString(dir, false)
	if primary == fallback {
		return primary, nil
	}
	return primary + " || " + fallback, nil
}

func claudeCommandArgs(dir string, continueSession bool) []string {
	args := []string{"--dangerously-skip-permissions"}
	if sys := loadInstructionsFile(dir, "CLAUDE.md"); sys != "" {
		args = append(args, "--append-system-prompt", sys)
	}
	if continueSession {
		args = append(args, "--continue")
	}
	return args
}

func claudeCommandString(dir string, continueSession bool) string {
	args := claudeCommandArgs(dir, continueSession)
	quoted := make([]string, 0, len(args)+2)
	quoted = append(quoted, claudePromptSuggestionDisabledEnv, "claude")
	for _, arg := range args {
		quoted = append(quoted, shellQuote(arg))
	}
	return strings.Join(quoted, " ")
}
