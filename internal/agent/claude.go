package agent

import (
	"fmt"
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

func (claudeRuntime) Launch(dir, workspace, socketPath, axBin, configPath string, options LaunchOptions) error {
	if err := prepareClaudeLaunch(dir, options.FreshStart); err != nil {
		return err
	}

	if len(options.ExtraArgs) > 0 {
		return newClaudeCommand(dir, false, options.ExtraArgs).Run()
	}

	if options.FreshStart {
		return newClaudeCommand(dir, false, nil).Run()
	}

	cmd := newClaudeCommand(dir, true, nil)
	if err := cmd.Run(); err == nil {
		return nil
	}

	// Fallback without --continue
	fallback := newClaudeCommand(dir, false, nil)
	return fallback.Run()
}

func newClaudeCommand(dir string, continueSession bool, extraArgs []string) *exec.Cmd {
	args := claudeCommandArgs(dir, continueSession, extraArgs)
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

func prepareClaudeLaunch(dir string, fresh bool) error {
	if !fresh {
		return nil
	}

	projectPath, err := claudeProjectPath(dir)
	if err != nil {
		return err
	}
	if err := os.RemoveAll(projectPath); err != nil {
		return fmt.Errorf("remove claude project state %s: %w", projectPath, err)
	}
	return nil
}

func claudeProjectPath(dir string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolve home dir: %w", err)
	}
	projectKey := strings.NewReplacer("/", "-", ".", "-").Replace(filepath.Clean(dir))
	return filepath.Join(home, ".claude", "projects", projectKey), nil
}

func (claudeRuntime) UserCommand(dir, workspace, socketPath, axBin, configPath string, options LaunchOptions) (string, error) {
	if len(options.ExtraArgs) > 0 {
		return claudeCommandString(dir, false, options.ExtraArgs), nil
	}

	if options.FreshStart {
		return claudeCommandString(dir, false, nil), nil
	}

	primary := claudeCommandString(dir, true, nil)
	fallback := claudeCommandString(dir, false, nil)
	if primary == fallback {
		return primary, nil
	}
	return primary + " || " + fallback, nil
}

func claudeCommandArgs(dir string, continueSession bool, extraArgs []string) []string {
	args := []string{"--dangerously-skip-permissions"}
	if sys := loadInstructionsFile(dir, "CLAUDE.md"); sys != "" {
		args = append(args, "--append-system-prompt", sys)
	}
	if continueSession {
		args = append(args, "--continue")
	}
	args = append(args, extraArgs...)
	return args
}

func claudeCommandString(dir string, continueSession bool, extraArgs []string) string {
	args := claudeCommandArgs(dir, continueSession, extraArgs)
	quoted := make([]string, 0, len(args)+2)
	quoted = append(quoted, claudePromptSuggestionDisabledEnv, "claude")
	for _, arg := range args {
		quoted = append(quoted, shellQuote(arg))
	}
	return strings.Join(quoted, " ")
}
