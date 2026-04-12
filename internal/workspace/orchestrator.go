package workspace

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
)

// OrchestratorName is the fully-qualified identity of an orchestrator given
// its project prefix ("" for the root).
func OrchestratorName(prefix string) string {
	if prefix == "" {
		return "orchestrator"
	}
	return prefix + ".orchestrator"
}

// WriteOrchestratorPrompt generates a scope-specific instruction file for
// the orchestrator of a project. The root orchestrator learns about
// sub-orchestrators as delegation targets; sub-orchestrators learn about
// their parent for escalation.
func WriteOrchestratorPrompt(orchDir string, node *config.ProjectNode, prefix, parentName, runtime string) error {
	var sb strings.Builder

	selfName := OrchestratorName(prefix)
	isRoot := parentName == ""

	if isRoot {
		sb.WriteString("# ax root orchestrator\n\n")
		sb.WriteString(fmt.Sprintf("당신은 `%s` 프로젝트 트리의 루트 오케스트레이터입니다.\n", node.Name))
		sb.WriteString(fmt.Sprintf("당신의 ID는 `%s`입니다.\n\n", selfName))
	} else {
		sb.WriteString(fmt.Sprintf("# ax sub orchestrator: %s\n\n", node.Name))
		sb.WriteString(fmt.Sprintf("당신은 `%s` 프로젝트의 서브 오케스트레이터입니다.\n", node.Name))
		sb.WriteString(fmt.Sprintf("당신의 ID는 `%s`입니다.\n", selfName))
		sb.WriteString(fmt.Sprintf("상위 오케스트레이터: `%s`\n\n", parentName))
	}

	sb.WriteString("## 역할\n")
	if isRoot {
		sb.WriteString("- user의 요청을 받아 적절한 워크스페이스 또는 서브 오케스트레이터에게 분배합니다.\n")
		sb.WriteString("- 여러 프로젝트에 걸친 작업은 서브 오케스트레이터들을 조율합니다.\n")
		sb.WriteString("- 결과를 수집해 user에게 보고합니다.\n\n")
	} else {
		sb.WriteString(fmt.Sprintf("- `%s` 프로젝트 내부의 작업을 자체 워크스페이스들에게 분배합니다.\n", node.Name))
		sb.WriteString(fmt.Sprintf("- 상위 오케스트레이터(`%s`)로부터 오는 요청을 처리합니다.\n", parentName))
		sb.WriteString(fmt.Sprintf("- 프로젝트 범위를 벗어나는 요청은 `%s`에게 에스컬레이션합니다.\n", parentName))
		sb.WriteString("- 결과를 수집해 상위 오케스트레이터에게 보고합니다.\n\n")
	}

	sb.WriteString("## 행동 규칙\n")
	sb.WriteString("- read_messages를 주기적으로 확인하여 메시지를 처리하세요.\n")
	sb.WriteString("- 작업을 보낼 때는 send_message를 사용하세요.\n")
	if isRoot {
		sb.WriteString("- user에게 응답할 때는 send_message(to=\"user\")를 사용하세요.\n")
	} else {
		sb.WriteString(fmt.Sprintf("- 상위 오케스트레이터에게 응답할 때는 send_message(to=\"%s\")를 사용하세요.\n", parentName))
	}
	sb.WriteString("- 복잡한 작업은 단계별로 나누어 분배하세요.\n")
	sb.WriteString("- 작업 완료 후 품질을 확인하고, 필요하면 수정을 요청하세요.\n\n")

	// Direct workspaces (at this project level)
	if len(node.Workspaces) > 0 {
		sb.WriteString("## 직접 관리하는 워크스페이스\n\n")
		sb.WriteString("| 이름 | ID | 설명 |\n|---|---|---|\n")
		for _, ws := range node.Workspaces {
			desc := ws.Description
			if desc == "" {
				desc = "-"
			}
			sb.WriteString(fmt.Sprintf("| **%s** | `%s` | %s |\n", ws.Name, ws.MergedName, desc))
		}
		sb.WriteString("\n")
	}

	// Sub-orchestrators (one per child project)
	if len(node.Children) > 0 {
		sb.WriteString("## 서브 오케스트레이터 (프로젝트 단위 위임 대상)\n\n")
		sb.WriteString("| 프로젝트 | ID | 담당 |\n|---|---|---|\n")
		for _, child := range node.Children {
			childOrchID := OrchestratorName(child.Prefix)
			scope := summarizeWorkspaces(child)
			sb.WriteString(fmt.Sprintf("| **%s** | `%s` | %s |\n", child.Name, childOrchID, scope))
		}
		sb.WriteString("\n")
		if isRoot {
			sb.WriteString("프로젝트 범위 작업은 해당 서브 오케스트레이터에게 위임하세요. ")
			sb.WriteString("여러 프로젝트가 관련된 경우 서브 오케스트레이터들을 순차 조율하세요.\n\n")
		}
	}

	// Workspace instructions detail
	if len(node.Workspaces) > 0 {
		sb.WriteString("## 워크스페이스 상세 지침\n\n")
		for _, ws := range node.Workspaces {
			sb.WriteString(fmt.Sprintf("### %s (`%s`)\n", ws.Name, ws.MergedName))
			if ws.Description != "" {
				sb.WriteString("- " + ws.Description + "\n")
			}
			if ws.Instructions != "" {
				for _, line := range strings.Split(strings.TrimSpace(ws.Instructions), "\n") {
					sb.WriteString("  " + strings.TrimSpace(line) + "\n")
				}
			}
			sb.WriteString("\n")
		}
	}

	instructionFile, err := agent.InstructionFile(runtime)
	if err != nil {
		return err
	}
	path := filepath.Join(orchDir, instructionFile)
	for _, runtimeName := range agent.SupportedNames() {
		otherFile, err := agent.InstructionFile(runtimeName)
		if err != nil {
			return err
		}
		other := filepath.Join(orchDir, otherFile)
		if other != path {
			os.Remove(other)
		}
	}
	return os.WriteFile(path, []byte(sb.String()), 0o644)
}

func summarizeWorkspaces(node *config.ProjectNode) string {
	if node == nil {
		return "-"
	}
	names := make([]string, 0, len(node.Workspaces))
	for _, ws := range node.Workspaces {
		names = append(names, ws.Name)
	}
	if len(node.Children) > 0 {
		names = append(names, fmt.Sprintf("+%d sub-project(s)", len(node.Children)))
	}
	if len(names) == 0 {
		return "-"
	}
	return strings.Join(names, ", ")
}
