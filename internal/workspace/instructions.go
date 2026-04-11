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
%s`, axMarkerStart, workspace, strings.TrimSpace(instructions), axMarkerEnd)

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
