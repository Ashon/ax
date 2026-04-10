package cmd

import (
	"fmt"

	"github.com/ashon/amux/internal/agent"
	"github.com/spf13/cobra"
)

var runAgentRuntime string
var runAgentWorkspace string
var runAgentConfig string

var runAgentCmd = &cobra.Command{
	Use:    "run-agent",
	Short:  "Run a workspace agent process (used by amux-managed tmux sessions)",
	Hidden: true,
	RunE: func(cmd *cobra.Command, args []string) error {
		if runAgentWorkspace == "" {
			return fmt.Errorf("--workspace is required")
		}
		if _, err := agent.Get(runAgentRuntime); err != nil {
			return err
		}
		return agent.Run(runAgentRuntime, runAgentWorkspace, socketPath, runAgentConfig)
	},
}

func init() {
	runAgentCmd.Flags().StringVar(&runAgentRuntime, "runtime", agent.RuntimeClaude, "agent runtime (claude or codex)")
	runAgentCmd.Flags().StringVar(&runAgentWorkspace, "workspace", "", "workspace name")
	runAgentCmd.Flags().StringVar(&runAgentConfig, "config", "", "root amux config path")
	runAgentCmd.MarkFlagRequired("workspace")
	rootCmd.AddCommand(runAgentCmd)
}
