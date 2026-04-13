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
	sb.WriteString("- **위임은 항상 `send_message`로** 하세요. `request` 툴은 블로킹이라 여러 워크스페이스에 순차 호출하면 타임아웃이 쌓여 매우 느려집니다.\n")
	sb.WriteString("- 여러 워크스페이스에 동시에 일을 보낼 때는 `send_message`를 연속해서 호출하고(병렬 dispatch), 이후 `read_messages`로 응답을 수집하세요.\n")
	if isRoot {
		sb.WriteString("- user에게 응답할 때는 `send_message(to=\"user\")`를 사용하세요.\n")
	} else {
		sb.WriteString(fmt.Sprintf("- **상위 오케스트레이터(`%s`)로부터 메시지를 받으면**, 자체 워크스페이스들에게 `send_message`로 병렬 분배하고, 응답을 수집한 뒤 **즉시** `send_message(to=\"%s\")`로 요약 결과를 반드시 회신하세요. 회신 없이 유휴 상태로 들어가면 안 됩니다.\n", parentName, parentName))
		sb.WriteString(fmt.Sprintf("- 추가 작업 지시 없이 받은 요청이 완료되면 바로 `send_message(to=\"%s\")`로 완료 보고하세요.\n", parentName))
	}
	sb.WriteString("- 복잡한 작업은 단계별로 나누어 분배하세요.\n")
	sb.WriteString("- 작업 완료 후 품질을 확인하고, 필요하면 수정을 요청하세요.\n\n")

	sb.WriteString("## 위임 전용 원칙 (중요)\n")
	sb.WriteString("오케스트레이터는 **절대 직접 코드를 읽거나, 수정하거나, 파일을 생성하지 않습니다.** 모든 코딩 작업은 담당 워크스페이스 에이전트에게 위임합니다.\n\n")
	sb.WriteString("### 역할 범위\n")
	sb.WriteString("오케스트레이터의 역할은 오직 다음 3가지입니다:\n")
	sb.WriteString("1. **작업 분석 및 분배** — 요청을 분석하고 적절한 워크스페이스에 할당\n")
	sb.WriteString("2. **에이전트 간 조율** — 여러 워크스페이스 간 협업 조정\n")
	sb.WriteString("3. **결과 수집 및 보고** — 에이전트들의 결과를 취합하여 보고\n\n")
	sb.WriteString("### 위임 규칙\n")
	sb.WriteString("- 코드 변경이 필요한 작업 → 해당 워크스페이스 에이전트에게 `send_message`로 위임\n")
	sb.WriteString("- 여러 워크스페이스에 걸친 작업 → 각 에이전트에게 병렬 위임 후 `read_messages`로 결과 수집\n")
	sb.WriteString("- 코드 조사가 필요한 경우에도 직접 파일을 읽지 말고 에이전트에게 조사를 요청\n\n")
	sb.WriteString("### 도구 사용 제한\n")
	sb.WriteString("- **사용 가능**: ax MCP 도구만 사용합니다 (`send_message`, `read_messages`, `list_workspaces`, `set_status`, `create_task`, `update_task`, `get_task`, `list_tasks` 등)\n")
	sb.WriteString("- **사용 금지**: `Read`, `Edit`, `Write`, `Bash`, `Grep`, `Glob` 등 코드/파일 관련 도구는 사용하지 않습니다\n\n")

	sb.WriteString("## 응답 종결 규칙 (중요)\n")
	sb.WriteString("ACK 루프를 방지하기 위해 다음을 반드시 지키세요:\n")
	sb.WriteString("- **단순 확인/수신(ACK) 메시지를 보내지 마세요.** `[ack]`, `[received]`, `\"잘 받았습니다\"` 같은 내용만의 메시지는 절대 보내지 않습니다.\n")
	sb.WriteString("- 메시지에 **새로운 작업/정보가 포함되지 않았다면** 회신하지 마세요 (대화 종료).\n")
	sb.WriteString("- `request` 툴의 결과는 도구 반환값으로 받은 것이지 새 메시지가 아닙니다. 그 응답을 받았다고 해서 다시 메시지를 보내지 마세요.\n")
	sb.WriteString("- 작업 완료 보고를 보낸 후에는 상대의 확인/감사 메시지가 오더라도 다시 회신하지 마세요.\n")
	sb.WriteString("- 상태 알림은 `set_status`를 사용하고, `send_message`로 상태 핑을 보내지 마세요.\n\n")

	sb.WriteString("## 작업 관리 (Task Management)\n")
	sb.WriteString("워크스페이스에 작업을 위임할 때 task를 활용하여 진행 상황을 추적하세요.\n\n")
	sb.WriteString("### 오케스트레이터 워크플로우\n")
	sb.WriteString("1. 작업 위임 시 `create_task`로 task를 생성하고, `send_message`에 task ID를 포함하여 전달\n")
	sb.WriteString("2. `list_tasks`로 전체 진행 상황을 모니터링 (필터: `--assignee`, `--status`, `--created_by`)\n")
	sb.WriteString("3. `get_task`로 특정 작업의 상세 로그 확인\n\n")
	sb.WriteString("### 워크스페이스 에이전트에게 전달할 규칙\n")
	sb.WriteString("작업 위임 시 다음 안내를 메시지에 포함하세요:\n")
	sb.WriteString("- 작업 시작 시 `update_task(id=..., status=\"in_progress\")`로 상태 변경\n")
	sb.WriteString("- 주요 단계 완료 시 `update_task(id=..., log=\"진행 내용\")`으로 진행 로그 기록\n")
	sb.WriteString("- 작업 완료 시 `update_task(id=..., status=\"completed\", result=\"결과 요약\")`\n")
	sb.WriteString("- 작업 실패 시 `update_task(id=..., status=\"failed\", result=\"실패 원인\")`\n\n")

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
