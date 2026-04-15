package daemon

import (
	"strings"
	"testing"
)

func TestWakePromptIncludesOnlyDynamicDispatchHints(t *testing.T) {
	prompt := WakePrompt("ax.orchestrator", false)

	for _, want := range []string{
		"`read_messages`",
		`send_message(to="ax.orchestrator")`,
	} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("wake prompt missing %q: %q", want, prompt)
		}
	}

	for _, unwanted := range []string{
		"ACK",
		"set_status",
		"repeated summary/repeated confirmation",
		"`update_task(..., status=\"in_progress\"",
		"owner mismatch",
		"concise current-status re-ask",
	} {
		if strings.Contains(prompt, unwanted) {
			t.Fatalf("wake prompt unexpectedly contains %q: %q", unwanted, prompt)
		}
	}
}

func TestWakePromptIncludesFreshContextInstructions(t *testing.T) {
	prompt := WakePrompt("ax.orchestrator", true)

	for _, want := range []string{
		"fresh-context",
		"`Task ID:`",
		"`get_task`",
		"이전 대화 문맥을 이어받았다고 가정하지 말고",
	} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("fresh wake prompt missing %q: %q", want, prompt)
		}
	}
}
