package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/workspace"
	"github.com/spf13/cobra"
)

const claudeTmuxWorkspaceBase = "orchestrator-claude"

var claudeRunInTmux bool

type claudeLaunchSpec struct {
	Dir       string
	Workspace string
}

var claudeCmd = &cobra.Command{
	Use:   "claude",
	Short: "Launch Claude Code with the root orchestrator configuration",
	Long:  "Runs Claude Code directly against the root orchestrator prompt/MCP setup without opening the watch UI.",
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
		spec, err := prepareRootClaudeLaunch(tree, cfgPath, sp, isDaemonRunning(sp))
		if err != nil {
			return err
		}

		if !claudeRunInTmux {
			return agent.RunInDir(agent.RuntimeClaude, spec.Dir, spec.Workspace, sp, cfgPath)
		}

		command, err := agent.BuildUserCommand(agent.RuntimeClaude, spec.Dir, spec.Workspace, sp, "", cfgPath)
		if err != nil {
			return err
		}

		tmuxWorkspace := nextClaudeTmuxWorkspace(claudeTmuxWorkspaceBase)
		if err := tmux.CreateSessionWithCommand(tmuxWorkspace, spec.Dir, command); err != nil {
			return fmt.Errorf("create tmux session: %w", err)
		}
		return tmux.AttachSession(tmuxWorkspace)
	},
}

func prepareRootClaudeLaunch(tree *config.ProjectNode, cfgPath, socketPath string, writeMCP bool) (claudeLaunchSpec, error) {
	if tree == nil {
		return claudeLaunchSpec{}, fmt.Errorf("config tree is empty")
	}

	orchDir, err := orchestratorDir(tree)
	if err != nil {
		return claudeLaunchSpec{}, fmt.Errorf("resolve orchestrator dir: %w", err)
	}
	if err := os.MkdirAll(orchDir, 0o755); err != nil {
		return claudeLaunchSpec{}, fmt.Errorf("create orchestrator dir %s: %w", orchDir, err)
	}
	if err := os.MkdirAll(filepath.Join(orchDir, ".claude"), 0o755); err != nil {
		return claudeLaunchSpec{}, fmt.Errorf("create .claude dir: %w", err)
	}

	promptPath := filepath.Join(orchDir, "CLAUDE.md")
	prompt := workspace.OrchestratorPrompt(tree, tree.Prefix, "")
	if err := os.WriteFile(promptPath, []byte(prompt), 0o644); err != nil {
		return claudeLaunchSpec{}, fmt.Errorf("write %s: %w", promptPath, err)
	}

	if writeMCP {
		workspaceName := workspace.OrchestratorName(tree.Prefix)
		if err := workspace.WriteMCPConfig(orchDir, workspaceName, socketPath, cfgPath); err != nil {
			return claudeLaunchSpec{}, fmt.Errorf("write orchestrator mcp config: %w", err)
		}
	}

	return claudeLaunchSpec{
		Dir:       orchDir,
		Workspace: workspace.OrchestratorName(tree.Prefix),
	}, nil
}

func nextClaudeTmuxWorkspace(base string) string {
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
	claudeCmd.Flags().BoolVar(&claudeRunInTmux, "tmux", false, "launch Claude Code in a new tmux session")
	rootCmd.AddCommand(claudeCmd)
}
