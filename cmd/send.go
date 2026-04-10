package cmd

import (
	"fmt"
	"strings"

	"github.com/ashon/amux/internal/mcpserver"
	"github.com/spf13/cobra"
)

var sendCmd = &cobra.Command{
	Use:   "send <workspace> <message...>",
	Short: "Send a message to a workspace agent",
	Args:  cobra.MinimumNArgs(2),
	RunE: func(cmd *cobra.Command, args []string) error {
		to := args[0]
		message := strings.Join(args[1:], " ")

		client := mcpserver.NewDaemonClient(socketPath, "_cli")
		if err := client.Connect(); err != nil {
			return fmt.Errorf("connect to daemon: %w (is daemon running?)", err)
		}
		defer client.Close()

		msgID, err := client.SendMessage(to, message)
		if err != nil {
			return fmt.Errorf("send: %w", err)
		}

		fmt.Printf("Message sent to %q (id: %s)\n", to, msgID)
		return nil
	},
}

func init() {
	rootCmd.AddCommand(sendCmd)
}
