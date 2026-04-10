package cmd

import (
	"fmt"
	"strings"

	"github.com/ashon/amux/internal/mcpserver"
	"github.com/ashon/amux/internal/tmux"
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

		msgID, err := client.SendMessage(to, message)
		if err != nil {
			return fmt.Errorf("send: %w", err)
		}

		fmt.Printf("Message sent to %q (id: %s)\n", to, msgID)

		// Wake the agent after clearing any draft/multiline composer state.
		if tmux.SessionExists(to) {
			prompt := "read_messages MCP 도구로 수신 메시지를 확인하고 요청된 작업을 수행해 줘. 결과는 send_message(to=\"orchestrator\")로 보내줘."
			if err := tmux.WakeWorkspace(to, prompt); err != nil {
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
