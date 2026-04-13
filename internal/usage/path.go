package usage

import (
	"os"
	"path/filepath"
	"strings"
)

// ProjectDirFromCwd computes the Claude Code project directory name for a
// given absolute cwd. Rule (verified against ~/.claude/projects): replace
// every "/" and "." with "-". The leading "/" therefore produces a leading
// "-". Example: /Users/ashon/.ax/orchestrator -> -Users-ashon--ax-orchestrator.
func ProjectDirFromCwd(cwd string) string {
	r := strings.NewReplacer("/", "-", ".", "-")
	return r.Replace(cwd)
}

// ProjectPath returns the absolute path of the Claude Code project dir for a cwd.
func ProjectPath(cwd string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".claude", "projects", ProjectDirFromCwd(cwd)), nil
}
