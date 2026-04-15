package workspace

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/ashon/ax/internal/agent"
)

const (
	axMarkerStart = "<!-- ax:instructions:start -->"
	axMarkerEnd   = "<!-- ax:instructions:end -->"
)

func WriteInstructions(dir, workspace, runtime, instructions string) error {
	targetFile, err := agent.InstructionFile(runtime)
	if err != nil {
		return err
	}
	targetPath := filepath.Join(dir, targetFile)
	for _, runtimeName := range agent.SupportedNames() {
		file, err := agent.InstructionFile(runtimeName)
		if err != nil {
			return err
		}
		path := filepath.Join(dir, file)
		if path == targetPath {
			continue
		}
		removeInstructionsFile(path)
	}

	axSection := fmt.Sprintf(`%s
## ax workspace: %s

%s
%s`, axMarkerStart, workspace, managedWorkspaceInstructions(instructions), axMarkerEnd)

	existing, err := os.ReadFile(targetPath)
	if err != nil {
		// No existing file — write fresh
		return os.WriteFile(targetPath, []byte(axSection+"\n"), 0o644)
	}

	content := string(existing)
	startIdx := strings.Index(content, axMarkerStart)
	endIdx := strings.Index(content, axMarkerEnd)

	if startIdx >= 0 && endIdx >= 0 {
		// Replace existing ax section
		content = content[:startIdx] + axSection + content[endIdx+len(axMarkerEnd):]
	} else {
		// Append ax section
		content = strings.TrimRight(content, "\n") + "\n\n" + axSection + "\n"
	}

	return os.WriteFile(targetPath, []byte(content), 0o644)
}

func managedWorkspaceInstructions(instructions string) string {
	sections := make([]string, 0, 4)
	if trimmed := strings.TrimSpace(instructions); trimmed != "" {
		sections = append(sections, trimmed)
	}
	sections = append(sections, messageHandlingInstructionContract())
	sections = append(sections, taskIntakeInstructionContract())
	sections = append(sections, completionReportingInstructionContract())
	return strings.Join(sections, "\n\n")
}

func messageHandlingInstructionContract() string {
	return strings.Join([]string{
		"## Message Handling Contract",
		"- 수신 작업을 처리할 때는 `read_messages`로 최신 메시지를 확인하고, 새 작업 요청, 명시적 질문, 새 사실, 요청한 증거가 있을 때만 회신하세요.",
		"- 결과나 추가 정보가 필요할 때만 `send_message`로 회신하세요. 단순 ACK/수신 확인/감사/상태 핑만의 메시지는 보내지 마세요.",
		"- 진행 상태 공유가 필요하면 `send_message` 대신 `set_status`를 사용하세요.",
		"- 처리 결과는 현재 작업을 요청한 발신자에게만 보내고, 새 작업/새 사실/명시적 질문/요청한 증거가 없으면 침묵을 기본값으로 두세요.",
		"- `read_messages`에서 받은 최신 메시지가 이전에 처리한 메시지와 실질적으로 동일하거나, 지금 보내려는 응답이 이전 응답과 실질적으로 동일하면 회신하지 마세요.",
		"- `\"no new work\"`, `\"nothing to do\"`, `\"대기 중\"`, `\"진행 상황 없음\"`, `\"확인했습니다\"`, `\"thanks\"`, `\"ok\"` 같은 no-op 상태 메시지에는 회신하지 마세요.",
		"- `request` 툴의 반환값은 새 메시지가 아닙니다. 그 결과를 받았다고 다시 `send_message`를 보내지 마세요.",
	}, "\n")
}

func taskIntakeInstructionContract() string {
	return strings.Join([]string{
		"## Task Intake Contract",
		"- 메시지에 `Task ID:`가 있으면, 전달되었거나 `read_messages`로 읽었다는 사실만으로 task를 claim한 것으로 간주하지 마세요.",
		"- 먼저 `get_task`로 task 문맥을 확인하세요.",
		"- 그 직후 첫 task-flow action은 정확히 다음 4가지 중 하나여야 합니다:",
		"  1. `update_task(..., status=\"in_progress\", log=\"mode=implementation|inspection; scope=<exact files/modules>; validation=<plan>\")`",
		"  2. exact blocker 또는 owner mismatch 보고",
		"  3. superseded/invalid/fail 명시 후 종료",
		"  4. structured evidence와 함께 completion",
		"- owner mismatch나 missing dependency가 보이면 fail fast 하세요. 다른 owner/API/file이 필요한지 구체적으로 적고 task를 오래 붙잡지 마세요.",
		"- 같은 `Task ID:`에 대해 substantive result를 이미 보냈다면, 그 뒤 도착한 concise current-status re-ask에는 같은 요약을 반복하지 말고 새 delta가 있을 때만 회신하세요.",
	}, "\n")
}

func completionReportingInstructionContract() string {
	return strings.Join([]string{
		"## Completion Reporting Contract",
		"- `update_task(..., status=\"completed\", result=...)` 또는 completion 회신 전에는 현재 scope 기준으로 남은 owned dirty/uncommitted files가 있는지 확인하세요.",
		"- completion result에는 반드시 다음 둘 중 하나를 포함하세요: `remaining owned dirty files=<none>` 또는 `remaining owned dirty files=<paths>; residual scope=<why work remains>`.",
		"- commit/task slice만 끝났다면 전체 요청이 끝난 것처럼 쓰지 말고, 이번에 끝난 unit과 남은 owned work를 구분해서 적으세요.",
		"- leftover owned work가 남아 있는데 설명 없이 `completed`나 \"done\"처럼 쓰지 마세요. 후속 unit, 범위 밖 항목, blocker 중 무엇인지 명시하세요.",
	}, "\n")
}

func RemoveInstructions(dir string) {
	for _, runtimeName := range agent.SupportedNames() {
		file, err := agent.InstructionFile(runtimeName)
		if err != nil {
			continue
		}
		path := filepath.Join(dir, file)
		removeInstructionsFile(path)
	}
}

func removeInstructionsFile(path string) {
	data, err := os.ReadFile(path)
	if err != nil {
		return
	}

	content := string(data)
	startIdx := strings.Index(content, axMarkerStart)
	endIdx := strings.Index(content, axMarkerEnd)

	if startIdx < 0 || endIdx < 0 {
		return
	}

	// Remove the ax section and surrounding blank lines
	before := strings.TrimRight(content[:startIdx], "\n")
	after := strings.TrimLeft(content[endIdx+len(axMarkerEnd):], "\n")

	if before == "" && after == "" {
		os.Remove(path)
		return
	}

	result := before
	if after != "" {
		if result != "" {
			result += "\n\n"
		}
		result += after
	}
	os.WriteFile(path, []byte(result+"\n"), 0o644)
}
