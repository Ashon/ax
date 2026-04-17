package cmd

import (
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/mcpserver"
	"github.com/spf13/cobra"
)

type sendClient interface {
	Connect() error
	Close() error
	SendMessage(to, message, configPath string) (*mcpserver.SendMessageResult, error)
}

var (
	sendNewClient         = func(socketPath, workspace string) sendClient { return mcpserver.NewDaemonClient(socketPath, workspace) }
	sendResolveConfigPath = resolveConfigPath
)

var sendCmd = &cobra.Command{
	Use:   "send <workspace> <message...>",
	Short: "Send a message to a workspace agent and dispatch it on demand",
	Long:  "Sends a message via the daemon (recorded in history) and ensures the target agent session exists before nudging it to process the queued work.",
	Args:  cobra.MinimumNArgs(2),
	RunE: func(cmd *cobra.Command, args []string) error {
		to := args[0]
		message := strings.Join(args[1:], " ")

		cfgPath, err := sendResolveConfigPath()
		if err != nil {
			return fmt.Errorf("resolve dispatch config: %w", err)
		}

		client := sendNewClient(socketPath, "orchestrator")
		if err := client.Connect(); err != nil {
			return fmt.Errorf("connect to daemon: %w (is daemon running?)", err)
		}
		defer client.Close()

		sendResult, err := client.SendMessage(to, message, cfgPath)
		if err != nil {
			return fmt.Errorf("send: %w", err)
		}

		if sendResult.Suppressed {
			fmt.Printf("Message to %q suppressed as a duplicate no-op/status update.\n", to)
			return nil
		}

		fmt.Printf("Message sent to %q (id: %s)\n", to, sendResult.MessageID)
		fmt.Printf("Agent %q readied for queued work.\n", to)

		return nil
	},
}

func init() {
	rootCmd.AddCommand(sendCmd)
}
