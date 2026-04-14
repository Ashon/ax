package agent

import (
	"crypto/sha1"
	"encoding/hex"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/ashon/ax/internal/config"
)

func PrepareCodexHome(workspace, dir, socketPath, axBin, configPath string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolve home dir: %w", err)
	}

	codexHome, err := CodexHomePath(workspace, dir)
	if err != nil {
		return "", err
	}
	if err := os.MkdirAll(codexHome, 0o755); err != nil {
		return "", fmt.Errorf("create codex home: %w", err)
	}

	if err := linkIfPresent(filepath.Join(home, ".codex", "auth.json"), filepath.Join(codexHome, "auth.json")); err != nil {
		return "", err
	}

	content, err := loadBaseCodexConfig(filepath.Join(home, ".codex", "config.toml"))
	if err != nil {
		return "", err
	}

	content = upsertTopLevelKey(content, "model_reasoning_effort", strconv.Quote(resolveCodexReasoningEffort(configPath, workspace)))
	content = upsertKeyInSection(content, fmt.Sprintf("[projects.%s]", strconv.Quote(dir)), "trust_level", `"trusted"`)
	content = upsertKeyInSection(content, "[mcp_servers.ax]", "command", strconv.Quote(axBin))
	args := fmt.Sprintf(`["mcp-server","--workspace",%s,"--socket",%s`, strconv.Quote(workspace), strconv.Quote(socketPath))
	if configPath != "" {
		args += fmt.Sprintf(`,"--config",%s`, strconv.Quote(configPath))
	}
	args += `]`
	content = upsertKeyInSection(content, "[mcp_servers.ax]", "args", args)

	if err := os.WriteFile(filepath.Join(codexHome, "config.toml"), []byte(ensureTrailingNewline(content)), 0o644); err != nil {
		return "", fmt.Errorf("write codex config: %w", err)
	}

	return codexHome, nil
}

func CodexHomePath(workspace, dir string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolve home dir: %w", err)
	}
	return filepath.Join(home, ".ax", "codex", codexHomeKey(workspace, dir)), nil
}

func RemoveCodexHome(workspace, dir string) error {
	codexHome, err := CodexHomePath(workspace, dir)
	if err != nil {
		return err
	}
	if err := os.RemoveAll(codexHome); err != nil {
		return fmt.Errorf("remove codex home %s: %w", codexHome, err)
	}
	return nil
}

func codexHomeKey(workspace, dir string) string {
	sum := sha1.Sum([]byte(dir))
	return workspace + "-" + hex.EncodeToString(sum[:6])
}

func loadBaseCodexConfig(path string) (string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return "", nil
		}
		return "", fmt.Errorf("read base codex config: %w", err)
	}
	return string(data), nil
}

func linkIfPresent(src, dst string) error {
	info, err := os.Lstat(src)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("stat %s: %w", src, err)
	}
	if info.IsDir() {
		return nil
	}

	if existing, err := os.Lstat(dst); err == nil {
		if existing.Mode()&os.ModeSymlink != 0 {
			if target, err := os.Readlink(dst); err == nil && target == src {
				return nil
			}
		}
		if err := os.Remove(dst); err != nil {
			return fmt.Errorf("remove stale %s: %w", dst, err)
		}
	} else if !os.IsNotExist(err) {
		return fmt.Errorf("stat %s: %w", dst, err)
	}

	if err := os.Symlink(src, dst); err != nil {
		return fmt.Errorf("link %s -> %s: %w", dst, src, err)
	}
	return nil
}

func resolveCodexReasoningEffort(configPath, workspace string) string {
	if strings.TrimSpace(configPath) == "" {
		return config.DefaultCodexReasoningEffort
	}

	cfg, err := config.Load(configPath)
	if err != nil {
		return config.DefaultCodexReasoningEffort
	}
	return cfg.CodexReasoningEffortForWorkspace(workspace)
}

func upsertKeyInSection(content, header, key, value string) string {
	lines := splitLines(content)
	sectionStart := -1
	sectionEnd := len(lines)

	for i, line := range lines {
		if strings.TrimSpace(line) == header {
			sectionStart = i
			for j := i + 1; j < len(lines); j++ {
				trimmed := strings.TrimSpace(lines[j])
				if strings.HasPrefix(trimmed, "[") && strings.HasSuffix(trimmed, "]") {
					sectionEnd = j
					break
				}
			}
			break
		}
	}

	entry := fmt.Sprintf("%s = %s", key, value)
	if sectionStart == -1 {
		if len(lines) > 0 && strings.TrimSpace(lines[len(lines)-1]) != "" {
			lines = append(lines, "")
		}
		lines = append(lines, header, entry)
		return strings.Join(lines, "\n")
	}

	for i := sectionStart + 1; i < sectionEnd; i++ {
		if strings.HasPrefix(strings.TrimSpace(lines[i]), key+" ") || strings.HasPrefix(strings.TrimSpace(lines[i]), key+"=") {
			lines[i] = entry
			return strings.Join(lines, "\n")
		}
	}

	out := make([]string, 0, len(lines)+1)
	out = append(out, lines[:sectionEnd]...)
	out = append(out, entry)
	out = append(out, lines[sectionEnd:]...)
	return strings.Join(out, "\n")
}

func upsertTopLevelKey(content, key, value string) string {
	lines := splitLines(content)
	entry := fmt.Sprintf("%s = %s", key, value)

	for i, line := range lines {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") && strings.HasSuffix(trimmed, "]") {
			out := make([]string, 0, len(lines)+1)
			out = append(out, lines[:i]...)
			out = append(out, entry)
			out = append(out, lines[i:]...)
			return strings.Join(out, "\n")
		}
		if strings.HasPrefix(trimmed, key+" ") || strings.HasPrefix(trimmed, key+"=") {
			lines[i] = entry
			return strings.Join(lines, "\n")
		}
	}

	return strings.Join(append(lines, entry), "\n")
}

func splitLines(content string) []string {
	if content == "" {
		return nil
	}
	content = strings.TrimRight(content, "\n")
	if content == "" {
		return nil
	}
	return strings.Split(content, "\n")
}

func ensureTrailingNewline(content string) string {
	if content == "" || strings.HasSuffix(content, "\n") {
		return content
	}
	return content + "\n"
}
