package workspace

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const (
	amuxMarkerStart = "<!-- amux:instructions:start -->"
	amuxMarkerEnd   = "<!-- amux:instructions:end -->"
)

func WriteInstructions(dir, workspace, instructions string) error {
	path := filepath.Join(dir, "CLAUDE.md")

	amuxSection := fmt.Sprintf(`%s
## amux workspace: %s

%s
%s`, amuxMarkerStart, workspace, strings.TrimSpace(instructions), amuxMarkerEnd)

	existing, err := os.ReadFile(path)
	if err != nil {
		// No existing file — write fresh
		return os.WriteFile(path, []byte(amuxSection+"\n"), 0o644)
	}

	content := string(existing)
	startIdx := strings.Index(content, amuxMarkerStart)
	endIdx := strings.Index(content, amuxMarkerEnd)

	if startIdx >= 0 && endIdx >= 0 {
		// Replace existing amux section
		content = content[:startIdx] + amuxSection + content[endIdx+len(amuxMarkerEnd):]
	} else {
		// Append amux section
		content = strings.TrimRight(content, "\n") + "\n\n" + amuxSection + "\n"
	}

	return os.WriteFile(path, []byte(content), 0o644)
}

func RemoveInstructions(dir string) {
	path := filepath.Join(dir, "CLAUDE.md")

	data, err := os.ReadFile(path)
	if err != nil {
		return
	}

	content := string(data)
	startIdx := strings.Index(content, amuxMarkerStart)
	endIdx := strings.Index(content, amuxMarkerEnd)

	if startIdx < 0 || endIdx < 0 {
		return
	}

	// Remove the amux section and surrounding blank lines
	before := strings.TrimRight(content[:startIdx], "\n")
	after := strings.TrimLeft(content[endIdx+len(amuxMarkerEnd):], "\n")

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
