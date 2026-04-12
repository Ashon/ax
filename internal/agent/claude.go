package agent

import (
	"os"
	"os/exec"
	"path/filepath"
)

type claudeRuntime struct{}

func (claudeRuntime) Name() string {
	return RuntimeClaude
}

func (claudeRuntime) InstructionFile() string {
	return "CLAUDE.md"
}

func (claudeRuntime) Launch(dir, workspace, socketPath, axBin, configPath string) error {
	args := []string{"--dangerously-skip-permissions"}
	if sys := loadInstructionsFile(dir, "CLAUDE.md"); sys != "" {
		args = append(args, "--append-system-prompt", sys)
	}
	args = append(args, "--continue")

	cmd := exec.Command("claude", args...)
	cmd.Dir = dir
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err == nil {
		return nil
	}

	// Fallback without --continue
	fallbackArgs := []string{"--dangerously-skip-permissions"}
	if sys := loadInstructionsFile(dir, "CLAUDE.md"); sys != "" {
		fallbackArgs = append(fallbackArgs, "--append-system-prompt", sys)
	}
	fallback := exec.Command("claude", fallbackArgs...)
	fallback.Dir = dir
	fallback.Stdin = os.Stdin
	fallback.Stdout = os.Stdout
	fallback.Stderr = os.Stderr
	return fallback.Run()
}

func loadInstructionsFile(dir, name string) string {
	data, err := os.ReadFile(filepath.Join(dir, name))
	if err != nil {
		return ""
	}
	return string(data)
}

func (claudeRuntime) UserCommand(dir, workspace, socketPath, axBin, configPath string) (string, error) {
	return "claude", nil
}
