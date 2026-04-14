package cmd

import (
	"github.com/ashon/ax/internal/agent"
	"github.com/spf13/cobra"
)

var claudeCmd = &cobra.Command{
	Use:   "claude",
	Short: "Launch the Claude CLI as the root orchestrator",
	Long: "Runs the Claude coding-agent CLI in the foreground with the root " +
		"orchestrator's prompt and MCP configuration applied. The CLI " +
		"registers to the ax daemon as the \"orchestrator\" workspace, " +
		"so it can delegate to sub-orchestrators and workspaces. Requires " +
		"'ax up' or at least a running daemon; sub-orchestrator sessions " +
		"are started automatically if missing.",
	RunE: func(cmd *cobra.Command, args []string) error {
		return runRootOrchestrator(agent.RuntimeClaude)
	},
}

func init() {
	rootCmd.AddCommand(claudeCmd)
}
