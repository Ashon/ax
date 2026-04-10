package workspace

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/ashon/amux/internal/agent"
	"github.com/ashon/amux/internal/config"
)

const OrchestratorInstructions = "" // kept for backwards compat, use WriteOrchestratorPrompt instead

// WriteOrchestratorPrompt generates the runtime-specific instruction file for the orchestrator
// that includes the full workspace topology and collaboration rules.
func WriteOrchestratorPrompt(orchDir string, cfg *config.Config, runtime string) error {
	var sb strings.Builder

	sb.WriteString("# amux orchestrator\n\n")
	sb.WriteString("당신은 amux 멀티 에이전트 시스템의 오케스트레이터입니다.\n\n")

	sb.WriteString("## 역할\n")
	sb.WriteString("- user로부터 작업 요청을 받아 적절한 워크스페이스 에이전트에게 분배합니다.\n")
	sb.WriteString("- 에이전트들의 작업 결과를 수집하고 user에게 보고합니다.\n")
	sb.WriteString("- 여러 에이전트 간 협업이 필요한 작업을 조율합니다.\n")
	sb.WriteString("- 에이전트 간 충돌이나 의존성 문제를 해결합니다.\n\n")

	sb.WriteString("## 행동 규칙\n")
	sb.WriteString("- read_messages를 주기적으로 확인하여 user와 에이전트들의 메시지를 처리하세요.\n")
	sb.WriteString("- 작업을 에이전트에게 보낼 때는 send_message를 사용하세요.\n")
	sb.WriteString("- 결과를 user에게 보고할 때는 send_message(to=\"user\")를 사용하세요.\n")
	sb.WriteString("- 복잡한 작업은 단계별로 나누어 여러 에이전트에게 분배하세요.\n")
	sb.WriteString("- 에이전트 작업 완료 후 품질을 확인하고, 필요하면 수정을 요청하세요.\n\n")

	// Workspace topology
	sb.WriteString("## 워크스페이스 목록\n\n")
	sb.WriteString("| 워크스페이스 | 설명 | 디렉토리 |\n")
	sb.WriteString("|------------|------|----------|\n")
	for name, ws := range cfg.Workspaces {
		sb.WriteString(fmt.Sprintf("| **%s** | %s | `%s` |\n", name, ws.Description, ws.Dir))
	}
	sb.WriteString("\n")

	// Per-workspace capabilities
	sb.WriteString("## 에이전트별 역할 상세\n\n")
	for name, ws := range cfg.Workspaces {
		sb.WriteString(fmt.Sprintf("### %s\n", name))
		sb.WriteString(fmt.Sprintf("- 설명: %s\n", ws.Description))
		if ws.Instructions != "" {
			// Extract first few lines as summary
			lines := strings.Split(strings.TrimSpace(ws.Instructions), "\n")
			maxLines := 5
			if len(lines) < maxLines {
				maxLines = len(lines)
			}
			for _, line := range lines[:maxLines] {
				sb.WriteString(fmt.Sprintf("  %s\n", strings.TrimSpace(line)))
			}
		}
		sb.WriteString("\n")
	}

	// Collaboration patterns
	sb.WriteString("## 작업 분배 가이드\n\n")
	sb.WriteString("- **백엔드 API 작업** → udcd-backend\n")
	sb.WriteString("- **프론트엔드 UI 작업** → udcd-frontend\n")
	sb.WriteString("- **백엔드+프론트엔드 연동 작업** → udcd-backend 먼저 (API 구현) → udcd-frontend (UI 연동)\n")
	sb.WriteString("- **DNS/인증/벤치마크** → udcd-ops\n")
	sb.WriteString("- **K8s 매니페스트** → udc-k8s\n")
	sb.WriteString("- **인프라 프로비저닝** → fransible\n")
	sb.WriteString("- **문서 조회/업데이트** → docs\n")
	sb.WriteString("- **여러 영역에 걸친 작업** → 관련 에이전트들에게 순차적으로 분배, 의존관계 고려\n")

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
