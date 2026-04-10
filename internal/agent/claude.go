package agent

import (
	"os"
	"os/exec"
)

type claudeRuntime struct{}

func (claudeRuntime) Name() string {
	return RuntimeClaude
}

func (claudeRuntime) InstructionFile() string {
	return "CLAUDE.md"
}

func (claudeRuntime) Launch(dir, workspace, socketPath, amuxBin, configPath string) error {
	cmd := exec.Command("claude", "--dangerously-skip-permissions", "--continue")
	cmd.Dir = dir
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err == nil {
		return nil
	}

	fallback := exec.Command("claude", "--dangerously-skip-permissions")
	fallback.Dir = dir
	fallback.Stdin = os.Stdin
	fallback.Stdout = os.Stdout
	fallback.Stderr = os.Stderr
	return fallback.Run()
}

func (claudeRuntime) UserCommand(dir, workspace, socketPath, amuxBin, configPath string) (string, error) {
	return "claude", nil
}
