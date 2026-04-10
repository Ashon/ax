package cmd

import (
	"encoding/json"
	"fmt"
	"time"

	"github.com/ashon/amux/internal/mcpserver"
	"github.com/spf13/cobra"
)

var (
	msgFrom    string
	msgLimit   int
	msgWait    bool
	msgTimeout int
)

var messagesCmd = &cobra.Command{
	Use:     "messages",
	Aliases: []string{"msg"},
	Short:   "Read messages sent to the CLI orchestrator",
	RunE: func(cmd *cobra.Command, args []string) error {
		client := mcpserver.NewDaemonClient(socketPath, "_cli")
		if err := client.Connect(); err != nil {
			return fmt.Errorf("connect to daemon: %w (is daemon running?)", err)
		}
		defer client.Close()

		if !msgWait {
			return readAndPrint(client)
		}

		// Wait mode: poll until messages arrive
		deadline := time.Now().Add(time.Duration(msgTimeout) * time.Second)
		fmt.Println("Waiting for messages... (Ctrl+C to stop)")
		for time.Now().Before(deadline) {
			msgs, err := client.ReadMessages(msgLimit, msgFrom)
			if err != nil {
				return err
			}
			if len(msgs) > 0 {
				for _, msg := range msgs {
					printMessage(msg.From, msg.CreatedAt.Format("15:04:05"), msg.Content)
				}
				return nil
			}
			time.Sleep(2 * time.Second)
		}
		fmt.Println("No messages received within timeout.")
		return nil
	},
}

func readAndPrint(client *mcpserver.DaemonClient) error {
	msgs, err := client.ReadMessages(msgLimit, msgFrom)
	if err != nil {
		return err
	}
	if len(msgs) == 0 {
		fmt.Println("No messages.")
		return nil
	}
	for _, msg := range msgs {
		printMessage(msg.From, msg.CreatedAt.Format("15:04:05"), msg.Content)
	}
	return nil
}

func printMessage(from, timestamp, content string) {
	fmt.Printf("── [%s] from %s ──\n%s\n\n", timestamp, from, content)
}

// Also add a JSON output option for programmatic use
var msgJSON bool

var messagesJSONCmd = &cobra.Command{
	Use:    "messages-json",
	Hidden: true,
	Short:  "Read messages in JSON format",
	RunE: func(cmd *cobra.Command, args []string) error {
		client := mcpserver.NewDaemonClient(socketPath, "_cli")
		if err := client.Connect(); err != nil {
			return err
		}
		defer client.Close()

		msgs, err := client.ReadMessages(msgLimit, msgFrom)
		if err != nil {
			return err
		}
		data, _ := json.MarshalIndent(msgs, "", "  ")
		fmt.Println(string(data))
		return nil
	},
}

func init() {
	messagesCmd.Flags().StringVar(&msgFrom, "from", "", "filter by sender workspace")
	messagesCmd.Flags().IntVar(&msgLimit, "limit", 10, "max messages to read")
	messagesCmd.Flags().BoolVar(&msgWait, "wait", false, "wait for messages to arrive")
	messagesCmd.Flags().IntVar(&msgTimeout, "timeout", 120, "wait timeout in seconds")

	rootCmd.AddCommand(messagesCmd)
	rootCmd.AddCommand(messagesJSONCmd)
}
