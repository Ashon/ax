package cmd

import (
	"github.com/ashon/ax/internal/agent"
	"github.com/spf13/cobra"
)

var codexCmd = &cobra.Command{
	Use:                "codex [codex args...]",
	DisableFlagParsing: true,
	Short:              "Launch the Codex CLI as the root orchestrator",
	Long: "Runs the Codex coding-agent CLI in the foreground with the root " +
		"orchestrator's prompt and MCP configuration applied. The CLI " +
		"registers to the ax daemon as the \"orchestrator\" workspace, " +
		"so it can delegate to sub-orchestrators and workspaces. Requires " +
		"'ax up' or at least a running daemon; sub-orchestrator sessions " +
		"are started automatically if missing. Additional Codex arguments " +
		"are passed through after ax prepares the orchestrator context; put " +
		"ax flags before `codex` when combining them.",
	RunE: func(cmd *cobra.Command, args []string) error {
		return runRootOrchestrator(agent.RuntimeCodex, args)
	},
}

func init() {
	rootCmd.AddCommand(codexCmd)
}
