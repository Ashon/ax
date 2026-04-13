package cmd

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/ashon/ax/internal/types"
)

func TestFormatMessagesOutputText(t *testing.T) {
	output, err := formatMessagesOutput([]types.Message{
		{
			From:      "ax.orchestrator",
			Content:   "Task ready",
			CreatedAt: time.Date(2026, 4, 14, 2, 30, 0, 0, time.UTC),
		},
	}, false)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	for _, want := range []string{
		"── [02:30:00] from ax.orchestrator ──",
		"Task ready",
	} {
		if !strings.Contains(output, want) {
			t.Fatalf("expected %q in output %q", want, output)
		}
	}
}

func TestFormatMessagesOutputJSON(t *testing.T) {
	output, err := formatMessagesOutput([]types.Message{
		{
			ID:        "msg-1",
			From:      "ax.runtime",
			To:        cliInboxWorkspace,
			Content:   "Runtime result",
			CreatedAt: time.Date(2026, 4, 14, 2, 31, 0, 0, time.UTC),
		},
	}, true)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var msgs []types.Message
	if err := json.Unmarshal([]byte(output), &msgs); err != nil {
		t.Fatalf("expected valid json, got %q: %v", output, err)
	}
	if len(msgs) != 1 || msgs[0].ID != "msg-1" || msgs[0].To != cliInboxWorkspace {
		t.Fatalf("unexpected json payload: %+v", msgs)
	}
}

func TestMessagesHelpMentionsCLIInboxAndJSON(t *testing.T) {
	cmd := newMessagesCommand()
	var out bytes.Buffer
	cmd.SetOut(&out)
	cmd.SetErr(&out)
	cmd.SetArgs([]string{"--help"})

	if err := cmd.Execute(); err != nil {
		t.Fatalf("unexpected help error: %v", err)
	}

	help := out.String()
	for _, want := range []string{
		"CLI inbox",
		"_cli",
		"--json",
	} {
		if !strings.Contains(help, want) {
			t.Fatalf("expected %q in help output %q", want, help)
		}
	}
}

func TestTimeoutMessagesOutputJSON(t *testing.T) {
	if got := timeoutMessagesOutput(true); got != "[]\n" {
		t.Fatalf("expected JSON timeout output, got %q", got)
	}
	if got := timeoutMessagesOutput(false); got != "No messages received within timeout.\n" {
		t.Fatalf("expected text timeout output, got %q", got)
	}
}
