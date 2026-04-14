package cmd

import (
	"encoding/json"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/ashon/ax/internal/mcpserver"
	"github.com/ashon/ax/internal/types"
	"github.com/spf13/cobra"
)

const cliInboxWorkspace = "_cli"

var (
	msgFrom    string
	msgLimit   int
	msgWait    bool
	msgTimeout int
	msgJSON    bool
)

var messagesCmd = newMessagesCommand()
var messagesJSONCmd = newMessagesJSONCommand()

func newMessagesCommand() *cobra.Command {
	cmd := &cobra.Command{
		Use:     "messages",
		Aliases: []string{"msg"},
		Short:   "Read messages from the CLI inbox (_cli)",
		Long:    "Read messages queued for the ax CLI inbox identity `_cli`. Use text output by default, or `--json` for structured output.",
		RunE:    runMessagesCommand,
	}
	cmd.Flags().StringVar(&msgFrom, "from", "", "filter by sender workspace")
	cmd.Flags().IntVar(&msgLimit, "limit", 10, "max messages to read")
	cmd.Flags().BoolVar(&msgWait, "wait", false, "wait for messages to arrive")
	cmd.Flags().IntVar(&msgTimeout, "timeout", 120, "wait timeout in seconds")
	cmd.Flags().BoolVar(&msgJSON, "json", false, "print messages as JSON")
	return cmd
}

func newMessagesJSONCommand() *cobra.Command {
	cmd := &cobra.Command{
		Use:        "messages-json",
		Hidden:     true,
		Short:      "Read CLI inbox messages in JSON format",
		Deprecated: "use `ax messages --json` instead",
		RunE: func(cmd *cobra.Command, args []string) error {
			previous := msgJSON
			msgJSON = true
			defer func() { msgJSON = previous }()
			return runMessagesCommand(cmd, args)
		},
	}
	cmd.Flags().StringVar(&msgFrom, "from", "", "filter by sender workspace")
	cmd.Flags().IntVar(&msgLimit, "limit", 10, "max messages to read")
	cmd.Flags().BoolVar(&msgWait, "wait", false, "wait for messages to arrive")
	cmd.Flags().IntVar(&msgTimeout, "timeout", 120, "wait timeout in seconds")
	return cmd
}

func newCLIInboxClient() *mcpserver.DaemonClient {
	return mcpserver.NewDaemonClient(socketPath, cliInboxWorkspace)
}

func runMessagesCommand(cmd *cobra.Command, args []string) error {
	client := newCLIInboxClient()
	if err := client.Connect(); err != nil {
		return fmt.Errorf("connect to daemon: %w (is daemon running?)", err)
	}
	defer client.Close()

	if !msgWait {
		return readAndPrint(cmd.OutOrStdout(), client, msgJSON)
	}

	deadline := time.Now().Add(time.Duration(msgTimeout) * time.Second)
	fmt.Fprintln(cmd.OutOrStdout(), "Waiting for CLI inbox messages for `_cli`... (Ctrl+C to stop)")
	for time.Now().Before(deadline) {
		msgs, err := client.ReadMessages(msgLimit, msgFrom)
		if err != nil {
			return err
		}
		if len(msgs) > 0 {
			return writeMessagesOutput(cmd.OutOrStdout(), msgs, msgJSON)
		}
		time.Sleep(2 * time.Second)
	}

	_, err := io.WriteString(cmd.OutOrStdout(), timeoutMessagesOutput(msgJSON))
	return err
}

func readAndPrint(w io.Writer, client *mcpserver.DaemonClient, jsonOutput bool) error {
	msgs, err := client.ReadMessages(msgLimit, msgFrom)
	if err != nil {
		return err
	}
	return writeMessagesOutput(w, msgs, jsonOutput)
}

func writeMessagesOutput(w io.Writer, msgs []types.Message, jsonOutput bool) error {
	text, err := formatMessagesOutput(msgs, jsonOutput)
	if err != nil {
		return err
	}
	_, err = io.WriteString(w, text)
	return err
}

func formatMessagesOutput(msgs []types.Message, jsonOutput bool) (string, error) {
	if jsonOutput {
		data, err := json.MarshalIndent(msgs, "", "  ")
		if err != nil {
			return "", err
		}
		return string(data) + "\n", nil
	}
	if len(msgs) == 0 {
		return "No messages.\n", nil
	}

	var b strings.Builder
	for _, msg := range msgs {
		writeTextMessage(&b, msg.From, msg.CreatedAt.Format("15:04:05"), msg.Content)
	}
	return b.String(), nil
}

func timeoutMessagesOutput(jsonOutput bool) string {
	if jsonOutput {
		return "[]\n"
	}
	return "No messages received within timeout.\n"
}

func writeTextMessage(w io.Writer, from, timestamp, content string) {
	fmt.Fprintf(w, "── [%s] from %s ──\n%s\n\n", timestamp, from, content)
}

func init() {
	rootCmd.AddCommand(messagesCmd)
	rootCmd.AddCommand(messagesJSONCmd)
}
