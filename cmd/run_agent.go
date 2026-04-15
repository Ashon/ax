package cmd

import (
	"fmt"

	"github.com/ashon/ax/internal/agent"
	"github.com/spf13/cobra"
)

var runAgentRuntime string
var runAgentWorkspace string
var runAgentConfig string
var runAgentFresh bool

var runAgentCmd = &cobra.Command{
	Use:    "run-agent [-- extra-args...]",
	Short:  "Run a workspace agent process (used by ax-managed tmux sessions)",
	Hidden: true,
	RunE: func(cmd *cobra.Command, args []string) error {
		if runAgentWorkspace == "" {
			return fmt.Errorf("--workspace is required")
		}
		if _, err := agent.Get(runAgentRuntime); err != nil {
			return err
		}
		return agent.RunWithOptions(runAgentRuntime, runAgentWorkspace, socketPath, runAgentConfig, agent.LaunchOptions{
			FreshStart: runAgentFresh,
			ExtraArgs:  args,
		})
	},
}

func init() {
	runAgentCmd.Flags().StringVar(&runAgentRuntime, "runtime", agent.RuntimeClaude, "agent runtime (claude or codex)")
	runAgentCmd.Flags().StringVar(&runAgentWorkspace, "workspace", "", "workspace name")
	runAgentCmd.Flags().StringVar(&runAgentConfig, "config", "", "root ax config path")
	runAgentCmd.Flags().BoolVar(&runAgentFresh, "fresh", false, "reset runtime-owned context before launch")
	runAgentCmd.MarkFlagRequired("workspace")
	_ = runAgentCmd.Flags().MarkHidden("fresh")
	rootCmd.AddCommand(runAgentCmd)
}
