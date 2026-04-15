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
	sections := make([]string, 0, 2)
	if trimmed := strings.TrimSpace(instructions); trimmed != "" {
		sections = append(sections, trimmed)
	}
	sections = append(sections, taskIntakeInstructionContract())
	return strings.Join(sections, "\n\n")
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
