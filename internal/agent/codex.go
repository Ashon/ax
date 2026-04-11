package agent

import (
	"os"
	"os/exec"
)

type codexRuntime struct{}

func (codexRuntime) Name() string {
	return RuntimeCodex
}

func (codexRuntime) InstructionFile() string {
	return "AGENTS.md"
}

func (codexRuntime) Launch(dir, workspace, socketPath, axBin, configPath string) error {
	codexHome, err := PrepareCodexHome(workspace, dir, socketPath, axBin, configPath)
	if err != nil {
		return err
	}

	cmd := exec.Command("codex", "--dangerously-bypass-approvals-and-sandbox", "--no-alt-screen", "-C", dir)
	cmd.Env = append(os.Environ(), "CODEX_HOME="+codexHome)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

func (codexRuntime) UserCommand(dir, workspace, socketPath, axBin, configPath string) (string, error) {
	codexHome, err := PrepareCodexHome(workspace, dir, socketPath, axBin, configPath)
	if err != nil {
		return "", err
	}
	return "CODEX_HOME=" + shellQuote(codexHome) + " codex --dangerously-bypass-approvals-and-sandbox --no-alt-screen -C " + shellQuote(dir), nil
}
