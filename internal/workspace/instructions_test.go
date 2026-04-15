package workspace

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/agent"
)

func TestWriteInstructionsAppendsTaskEvidenceContract(t *testing.T) {
	dir := t.TempDir()
	file, err := agent.InstructionFile(agent.RuntimeCodex)
	if err != nil {
		t.Fatalf("instruction file: %v", err)
	}

	if err := WriteInstructions(dir, "ax.runtime", agent.RuntimeCodex, "Follow local ownership rules."); err != nil {
		t.Fatalf("write instructions: %v", err)
	}

	content, err := os.ReadFile(filepath.Join(dir, file))
	if err != nil {
		t.Fatalf("read instructions: %v", err)
	}
	text := string(content)
	for _, want := range []string{
		"Follow local ownership rules.",
		"## Task Intake Contract",
		"메시지에 `Task ID:`가 있으면, 전달되었거나 `read_messages`로 읽었다는 사실만으로 task를 claim한 것으로 간주하지 마세요.",
		"`get_task`로 task 문맥을 확인",
		"`update_task(..., status=\"in_progress\"",
		"owner mismatch나 missing dependency가 보이면 fail fast",
		"concise current-status re-ask에는 같은 요약을 반복하지 말고 새 delta가 있을 때만 회신",
	} {
		if !strings.Contains(text, want) {
			t.Fatalf("expected instructions to contain %q\n%s", want, text)
		}
	}
}

func TestWriteInstructionsReplacesManagedSectionWithoutDuplicatingContract(t *testing.T) {
	dir := t.TempDir()
	file, err := agent.InstructionFile(agent.RuntimeClaude)
	if err != nil {
		t.Fatalf("instruction file: %v", err)
	}

	target := filepath.Join(dir, file)
	if err := os.WriteFile(target, []byte("Existing intro\n"), 0o644); err != nil {
		t.Fatalf("seed instruction file: %v", err)
	}
	if err := WriteInstructions(dir, "ax.runtime", agent.RuntimeClaude, "First body."); err != nil {
		t.Fatalf("first write instructions: %v", err)
	}
	if err := WriteInstructions(dir, "ax.runtime", agent.RuntimeClaude, "Second body."); err != nil {
		t.Fatalf("second write instructions: %v", err)
	}

	content, err := os.ReadFile(target)
	if err != nil {
		t.Fatalf("read instructions: %v", err)
	}
	text := string(content)
	if strings.Contains(text, "First body.") {
		t.Fatalf("expected old managed instructions to be replaced\n%s", text)
	}
	if strings.Count(text, "## Task Intake Contract") != 1 {
		t.Fatalf("expected one task contract section, got %d\n%s", strings.Count(text, "## Task Intake Contract"), text)
	}
}
