package agent

import (
	"os"
	"os/exec"
	"strings"
)

type codexRuntime struct{}

func (codexRuntime) Name() string {
	return RuntimeCodex
}

func (codexRuntime) InstructionFile() string {
	return "AGENTS.md"
}

func (codexRuntime) Launch(dir, workspace, socketPath, axBin, configPath string, options LaunchOptions) error {
	codexHome, err := PrepareCodexHomeForLaunch(workspace, dir, socketPath, axBin, configPath, options.FreshStart)
	if err != nil {
		return err
	}

	cmd := exec.Command("codex", codexCommandArgs(dir, options.ExtraArgs)...)
	cmd.Env = append(os.Environ(), "CODEX_HOME="+codexHome)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

func (codexRuntime) UserCommand(dir, workspace, socketPath, axBin, configPath string, options LaunchOptions) (string, error) {
	codexHome, err := PrepareCodexHomeForLaunch(workspace, dir, socketPath, axBin, configPath, options.FreshStart)
	if err != nil {
		return "", err
	}

	parts := []string{"CODEX_HOME=" + shellQuote(codexHome), "codex"}
	for _, arg := range codexCommandArgs(dir, options.ExtraArgs) {
		parts = append(parts, shellQuote(arg))
	}
	return strings.Join(parts, " "), nil
}

func codexCommandArgs(dir string, extraArgs []string) []string {
	args := []string{"--dangerously-bypass-approvals-and-sandbox", "--no-alt-screen", "-C", dir}
	return append(args, extraArgs...)
}
