package cmd

import (
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/mcpserver"
	"github.com/ashon/ax/internal/tmux"
	"github.com/spf13/cobra"
)

var sendCmd = &cobra.Command{
	Use:   "send <workspace> <message...>",
	Short: "Send a message to a workspace agent and wake it",
	Long:  "Sends a message via the daemon (recorded in history) and wakes the agent via tmux to process it.",
	Args:  cobra.MinimumNArgs(2),
	RunE: func(cmd *cobra.Command, args []string) error {
		to := args[0]
		message := strings.Join(args[1:], " ")

		client := mcpserver.NewDaemonClient(socketPath, "orchestrator")
		if err := client.Connect(); err != nil {
			return fmt.Errorf("connect to daemon: %w (is daemon running?)", err)
		}
		defer client.Close()

		sendResult, err := client.SendMessage(to, message)
		if err != nil {
			return fmt.Errorf("send: %w", err)
		}

		if sendResult.Suppressed {
			fmt.Printf("Message to %q suppressed as a duplicate no-op/status update.\n", to)
			return nil
		}

		fmt.Printf("Message sent to %q (id: %s)\n", to, sendResult.MessageID)

		// Wake the agent after clearing any draft/multiline composer state.
		if tmux.SessionExists(to) {
			if err := tmux.WakeWorkspace(to, daemon.WakePrompt("orchestrator", false)); err != nil {
				return err
			}
			fmt.Printf("Agent %q woken up.\n", to)
		}

		return nil
	},
}

func init() {
	rootCmd.AddCommand(sendCmd)
}
