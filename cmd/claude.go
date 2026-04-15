package cmd

import (
	"github.com/ashon/ax/internal/agent"
	"github.com/spf13/cobra"
)

var claudeCmd = &cobra.Command{
	Use:                "claude [claude args...]",
	DisableFlagParsing: true,
	Short:              "Launch the Claude CLI as the root orchestrator",
	Long: "Runs the Claude coding-agent CLI in the foreground with the root " +
		"orchestrator's prompt and MCP configuration applied. The CLI " +
		"registers to the ax daemon as the \"orchestrator\" workspace, " +
		"so it can delegate to sub-orchestrators and workspaces. Requires " +
		"'ax up' or at least a running daemon; sub-orchestrator sessions " +
		"are started automatically if missing. Additional Claude arguments " +
		"are passed through after ax prepares the orchestrator context; put " +
		"ax flags before `claude` when combining them.",
	RunE: func(cmd *cobra.Command, args []string) error {
		return runRootOrchestrator(agent.RuntimeClaude, normalizeClaudePassthroughArgs(args))
	},
}

func init() {
	rootCmd.AddCommand(claudeCmd)
}

func normalizeClaudePassthroughArgs(args []string) []string {
	if len(args) == 0 {
		return nil
	}

	normalized := append([]string(nil), args...)
	switch normalized[0] {
	case "resume":
		normalized[0] = "--resume"
	case "continue":
		normalized[0] = "--continue"
	}
	return normalized
}
