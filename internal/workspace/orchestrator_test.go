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
