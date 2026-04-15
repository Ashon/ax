package daemon

import (
	"strings"
	"testing"
)

func TestWakePromptIncludesLoopGuard(t *testing.T) {
	prompt := WakePrompt("ax.orchestrator", false)

	for _, want := range []string{
		`send_message(to="ax.orchestrator")`,
		"ACK",
		"set_status",
		"새 작업 결과나 필요한 정보가 있을 때만 회신",
		"이전과 실질적으로 동일한 메시지",
		"이전 응답과 실질적으로 동일하면 회신하지",
		"repeated summary/repeated confirmation",
		"`update_task(..., status=\"in_progress\"",
		"owner mismatch",
		"concise current-status re-ask",
	} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("wake prompt missing %q: %q", want, prompt)
		}
	}
}

func TestWakePromptIncludesFreshContextInstructions(t *testing.T) {
	prompt := WakePrompt("ax.orchestrator", true)

	for _, want := range []string{
		"fresh-context",
		"`Task ID:`",
		"`get_task`",
		"`start_mode`가 `fresh`",
		"structured evidence와 함께 completion",
	} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("fresh wake prompt missing %q: %q", want, prompt)
		}
	}
}
