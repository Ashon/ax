package workspace

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
)

func TestWriteOrchestratorPromptDistinguishesAliasAndProjectName(t *testing.T) {
	instructionFile, err := agent.InstructionFile("claude")
	if err != nil {
		t.Fatalf("instruction file: %v", err)
	}

	child := &config.ProjectNode{
		Name:   "shared",
		Alias:  "alpha",
		Prefix: "alpha",
		Workspaces: []config.WorkspaceRef{
			{Name: "worker", MergedName: "alpha.worker"},
		},
	}

	subDir := t.TempDir()
	if err := WriteOrchestratorPrompt(subDir, child, child.Prefix, "orchestrator", "claude"); err != nil {
		t.Fatalf("write sub prompt: %v", err)
	}
	subPrompt, err := os.ReadFile(filepath.Join(subDir, instructionFile))
	if err != nil {
		t.Fatalf("read sub prompt: %v", err)
	}
	subText := string(subPrompt)
	for _, want := range []string{
		"# ax sub orchestrator: alpha (shared)",
		"당신은 `alpha (shared)` 프로젝트의 서브 오케스트레이터입니다.",
		"부모 트리에서의 별칭: `alpha`",
		"실제 프로젝트 이름: `shared`",
	} {
		if !strings.Contains(subText, want) {
			t.Fatalf("expected sub prompt to contain %q\n%s", want, subText)
		}
	}

	root := &config.ProjectNode{
		Name: "root",
		Children: []*config.ProjectNode{
			child,
		},
	}

	rootDir := t.TempDir()
	if err := WriteOrchestratorPrompt(rootDir, root, "", "", "claude"); err != nil {
		t.Fatalf("write root prompt: %v", err)
	}
	rootPrompt, err := os.ReadFile(filepath.Join(rootDir, instructionFile))
	if err != nil {
		t.Fatalf("read root prompt: %v", err)
	}
	if !strings.Contains(string(rootPrompt), "| **alpha (shared)** | `alpha.orchestrator` | worker |") {
		t.Fatalf("expected root prompt to list child display identity, got:\n%s", string(rootPrompt))
	}
}

func TestOrchestratorPromptRequiresTrackingAssignedWorkToClosure(t *testing.T) {
	root := &config.ProjectNode{Name: "root"}
	rootPrompt := OrchestratorPrompt(root, "", "")
	for _, want := range []string{
		"오케스트레이터는 자신이 assign한 일이 실제 완료 결과, 명시적 blocker 보고, 실패 중 하나의 종결 상태에 도달할 때까지 계속 추적할 책임이 있습니다.",
		"assign한 일은 실제 완료 증거를 받거나, blocker를 상위에 명시적으로 보고하거나, 실패로 종료할 때까지 계속 소유하고 추적합니다.",
	} {
		if !strings.Contains(rootPrompt, want) {
			t.Fatalf("expected root prompt to contain %q\n%s", want, rootPrompt)
		}
	}

	child := &config.ProjectNode{Name: "shared", Prefix: "shared"}
	subPrompt := OrchestratorPrompt(child, child.Prefix, "orchestrator")
	for _, want := range []string{
		"오케스트레이터는 자신이 assign한 일이 실제 완료 결과, 명시적 blocker 보고, 실패 중 하나의 종결 상태에 도달할 때까지 계속 추적할 책임이 있습니다.",
		"assign한 일은 실제 완료 증거를 받거나, blocker를 상위에 명시적으로 보고하거나, 실패로 종료할 때까지 계속 소유하고 추적합니다.",
	} {
		if !strings.Contains(subPrompt, want) {
			t.Fatalf("expected sub prompt to contain %q\n%s", want, subPrompt)
		}
	}
}
