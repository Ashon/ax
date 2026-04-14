package cmd

import (
	"fmt"
	"os"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/workspace"
	"github.com/spf13/cobra"
)

const codexTmuxWorkspaceBase = "orchestrator-codex"

var codexRunInTmux bool

type codexLaunchSpec struct {
	Dir       string
	Workspace string
}

var codexCmd = &cobra.Command{
	Use:   "codex",
	Short: "Launch Codex with the root orchestrator configuration",
	Long:  "Runs Codex directly against the root orchestrator prompt/MCP setup without opening the watch UI.",
	RunE: func(cmd *cobra.Command, args []string) error {
		cfgPath, err := resolveConfigPath()
		if err != nil {
			return err
		}

		tree, err := config.LoadTree(cfgPath)
		if err != nil {
			return fmt.Errorf("load config tree: %w", err)
		}

		sp := daemon.ExpandSocketPath(socketPath)
		spec, err := prepareRootCodexLaunch(tree, cfgPath, sp, isDaemonRunning(sp))
		if err != nil {
			return err
		}

		if !codexRunInTmux {
			return agent.RunInDir(agent.RuntimeCodex, spec.Dir, spec.Workspace, sp, cfgPath)
		}

		command, err := agent.BuildUserCommand(agent.RuntimeCodex, spec.Dir, spec.Workspace, sp, "", cfgPath)
		if err != nil {
			return err
		}

		tmuxWorkspace := nextCodexTmuxWorkspace(codexTmuxWorkspaceBase)
		if err := tmux.CreateSessionWithCommand(tmuxWorkspace, spec.Dir, command); err != nil {
			return fmt.Errorf("create tmux session: %w", err)
		}
		return tmux.AttachSession(tmuxWorkspace)
	},
}

func prepareRootCodexLaunch(tree *config.ProjectNode, cfgPath, socketPath string, writeMCP bool) (codexLaunchSpec, error) {
	if tree == nil {
		return codexLaunchSpec{}, fmt.Errorf("config tree is empty")
	}

	orchDir, err := orchestratorDir(tree)
	if err != nil {
		return codexLaunchSpec{}, fmt.Errorf("resolve orchestrator dir: %w", err)
	}
	if err := os.MkdirAll(orchDir, 0o755); err != nil {
		return codexLaunchSpec{}, fmt.Errorf("create orchestrator dir %s: %w", orchDir, err)
	}

	workspaceName := workspace.OrchestratorName(tree.Prefix)
	if writeMCP {
		if err := workspace.WriteMCPConfig(orchDir, workspaceName, socketPath, cfgPath); err != nil {
			return codexLaunchSpec{}, fmt.Errorf("write orchestrator mcp config: %w", err)
		}
	}
	if err := workspace.EnsureCodexConfig(orchDir, workspaceName, socketPath, cfgPath); err != nil {
		return codexLaunchSpec{}, fmt.Errorf("write orchestrator codex config: %w", err)
	}
	if err := workspace.WriteOrchestratorPrompt(orchDir, tree, tree.Prefix, "", agent.RuntimeCodex); err != nil {
		return codexLaunchSpec{}, fmt.Errorf("write orchestrator prompt: %w", err)
	}

	return codexLaunchSpec{
		Dir:       orchDir,
		Workspace: workspaceName,
	}, nil
}

func nextCodexTmuxWorkspace(base string) string {
	if !tmux.SessionExists(base) {
		return base
	}
	for i := 2; ; i++ {
		candidate := fmt.Sprintf("%s-%d", base, i)
		if !tmux.SessionExists(candidate) {
			return candidate
		}
	}
}

func init() {
	codexCmd.Flags().BoolVar(&codexRunInTmux, "tmux", false, "launch Codex in a new tmux session")
	rootCmd.AddCommand(codexCmd)
}
