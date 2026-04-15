package tmux

import (
	"fmt"
	"os"
	"os/exec"
	"sort"
	"strings"
	"time"
)

const SessionPrefix = "ax-"

func SessionName(workspace string) string {
	return SessionPrefix + encodeWorkspaceName(workspace)
}

type SessionInfo struct {
	Name      string
	Workspace string
	Attached  bool
	Windows   int
}

func CreateSession(workspace, dir, shell string) error {
	return CreateSessionWithEnv(workspace, dir, shell, nil)
}

func CreateSessionWithEnv(workspace, dir, shell string, env map[string]string) error {
	name := SessionName(workspace)

	args := []string{"new-session", "-d", "-s", name, "-c", dir}
	args = append(args, envArgs(env)...)
	if shell != "" {
		args = append(args, shell)
	}

	cmd := exec.Command("tmux", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux new-session: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

// CreateSessionWithCommand creates a tmux session that runs a command directly
// instead of starting a shell. The command replaces the shell process so no
// shell prompt is visible.
func CreateSessionWithCommand(workspace, dir, command string) error {
	return CreateSessionWithCommandEnv(workspace, dir, command, nil)
}

func CreateSessionWithCommandEnv(workspace, dir, command string, env map[string]string) error {
	name := SessionName(workspace)

	// Run the configured command through the shell so existing agent command
	// strings keep their current shell semantics.
	args := []string{"new-session", "-d", "-s", name, "-c", dir}
	args = append(args, commandWithEnv([]string{"sh", "-c", command}, env)...)
	cmd := exec.Command("tmux", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux new-session: %s: %w", strings.TrimSpace(string(out)), err)
	}

	return setRemainOnExit(name)
}

// CreateEphemeralSession creates a tmux session that runs a command directly
// and is destroyed automatically when the command exits. Unlike
// CreateSessionWithArgs it does NOT set remain-on-exit, so the session
// disappears as soon as the process terminates.
func CreateEphemeralSession(workspace, dir string, argv []string) error {
	name := SessionName(workspace)
	args := []string{"new-session", "-d", "-s", name, "-c", dir}
	args = append(args, argv...)
	cmd := exec.Command("tmux", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux new-session: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

func CreateSessionWithArgs(workspace, dir string, argv []string) error {
	return CreateSessionWithArgsEnv(workspace, dir, argv, nil)
}

func CreateSessionWithArgsEnv(workspace, dir string, argv []string, env map[string]string) error {
	name := SessionName(workspace)

	args := []string{"new-session", "-d", "-s", name, "-c", dir}
	args = append(args, commandWithEnv(argv, env)...)

	cmd := exec.Command("tmux", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux new-session: %s: %w", strings.TrimSpace(string(out)), err)
	}

	return setRemainOnExit(name)
}

func DestroySession(workspace string) error {
	name := SessionName(workspace)
	cmd := exec.Command("tmux", "kill-session", "-t", name)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux kill-session: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

func AttachSession(workspace string) error {
	name := SessionName(workspace)

	// If inside tmux, switch client; otherwise attach
	if isInsideTmux() {
		cmd := exec.Command("tmux", "switch-client", "-t", name)
		cmd.Stdin = os.Stdin
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr
		if err := cmd.Run(); err != nil {
			return fmt.Errorf("tmux switch-client: %w", err)
		}
	} else {
		cmd := exec.Command("tmux", "attach-session", "-t", name)
		cmd.Stdin = os.Stdin
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr
		if err := cmd.Run(); err != nil {
			return fmt.Errorf("tmux attach-session: %w", err)
		}
	}
	return nil
}

func ListSessions() ([]SessionInfo, error) {
	cmd := exec.Command("tmux", "list-sessions", "-F", "#{session_name} #{session_attached} #{session_windows}")
	out, err := cmd.CombinedOutput()
	return parseListSessionsResult(string(out), err)
}

func parseListSessionsResult(output string, err error) ([]SessionInfo, error) {
	if err != nil {
		// tmux reports this condition on stderr.
		if strings.Contains(output, "no server running") {
			return nil, nil
		}
		msg := strings.TrimSpace(output)
		if msg == "" {
			return nil, fmt.Errorf("tmux list-sessions: %w", err)
		}
		return nil, fmt.Errorf("tmux list-sessions: %s: %w", msg, err)
	}

	var sessions []SessionInfo
	for _, line := range strings.Split(strings.TrimSpace(output), "\n") {
		if line == "" {
			continue
		}
		parts := strings.Fields(line)
		if len(parts) != 3 {
			continue
		}
		name := parts[0]
		if !strings.HasPrefix(name, SessionPrefix) {
			continue
		}

		attached := parts[1] == "1"
		windows := 1
		fmt.Sscanf(parts[2], "%d", &windows)

		sessions = append(sessions, SessionInfo{
			Name:      name,
			Workspace: decodeWorkspaceName(strings.TrimPrefix(name, SessionPrefix)),
			Attached:  attached,
			Windows:   windows,
		})
	}
	return sessions, nil
}

func SessionExists(workspace string) bool {
	name := SessionName(workspace)
	cmd := exec.Command("tmux", "has-session", "-t", name)
	return cmd.Run() == nil
}

func SendSpecialKeys(workspace string, keys ...string) error {
	name := SessionName(workspace)
	args := []string{"send-keys", "-t", name}
	args = append(args, keys...)

	cmd := exec.Command("tmux", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux send-keys: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

// InterruptWorkspace asks the agent CLI to cancel its current interactive action
// without terminating the tmux session or shell process.
func InterruptWorkspace(workspace string) error {
	return SendKeys(workspace, []string{"Escape"})
}

// specialKeyMap maps user-friendly key names to tmux send-keys tokens.
// Anything not in this map is sent as literal text via the -l flag.
var specialKeyMap = map[string]string{
	"Enter": "Enter", "Return": "Enter",
	"Escape": "Escape", "Esc": "Escape",
	"Tab":       "Tab",
	"Space":     "Space",
	"BSpace":    "BSpace",
	"Backspace": "BSpace",
	"Delete":    "DC", "DC": "DC",
	"Up": "Up", "Down": "Down", "Left": "Left", "Right": "Right",
	"Home": "Home", "End": "End",
	"PageUp": "PPage", "PPage": "PPage",
	"PageDown": "NPage", "NPage": "NPage",
	"Ctrl-C": "C-c", "C-c": "C-c",
	"Ctrl-D": "C-d", "C-d": "C-d",
	"Ctrl-U": "C-u", "C-u": "C-u",
	"Ctrl-L": "C-l", "C-l": "C-l",
	"Ctrl-A": "C-a", "C-a": "C-a",
	"Ctrl-Z": "C-z", "C-z": "C-z",
	"Ctrl-R": "C-r", "C-r": "C-r",
	"Ctrl-W": "C-w", "C-w": "C-w",
	"Ctrl-K": "C-k", "C-k": "C-k",
	"Ctrl-E": "C-e", "C-e": "C-e",
	"Ctrl-B": "C-b", "C-b": "C-b",
	"Ctrl-F": "C-f", "C-f": "C-f",
	"Ctrl-P": "C-p", "C-p": "C-p",
	"Ctrl-N": "C-n", "C-n": "C-n",
}

// ResolveKeyToken returns the tmux send-keys token for a user-supplied key.
// The second return value is true if the key was recognized as a special
// (named) key; false means it should be treated as literal text.
func ResolveKeyToken(key string) (string, bool) {
	if mapped, ok := specialKeyMap[key]; ok {
		return mapped, true
	}
	return key, false
}

// SendKeys sends a sequence of keys to a workspace's tmux session. Each key
// is either a named special key (Enter, Escape, C-c, ...) or literal text.
// Named keys are resolved via ResolveKeyToken; unknown tokens are sent
// literally via tmux's -l flag so ordinary characters pass through unchanged.
// Returns an error if the session does not exist or any send-keys call fails.
func SendKeys(workspace string, keys []string) error {
	if !SessionExists(workspace) {
		return fmt.Errorf("tmux session for workspace %q not found", workspace)
	}
	name := SessionName(workspace)
	for _, k := range keys {
		if k == "" {
			continue
		}
		if mapped, isSpecial := ResolveKeyToken(k); isSpecial {
			cmd := exec.Command("tmux", "send-keys", "-t", name, mapped)
			if out, err := cmd.CombinedOutput(); err != nil {
				return fmt.Errorf("tmux send-keys %q: %s: %w", k, strings.TrimSpace(string(out)), err)
			}
		} else {
			cmd := exec.Command("tmux", "send-keys", "-t", name, "-l", k)
			if out, err := cmd.CombinedOutput(); err != nil {
				return fmt.Errorf("tmux send-keys literal %q: %s: %w", k, strings.TrimSpace(string(out)), err)
			}
		}
	}
	return nil
}

// WakeWorkspace nudges a Codex TUI session to process queued ax messages.
// Escape/C-u clears any draft or multiline composer state before the prompt is injected.
func WakeWorkspace(workspace, prompt string) error {
	name := SessionName(workspace)
	clearCmd := exec.Command("tmux", "send-keys", "-t", name, "Escape", "C-u")
	if out, err := clearCmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux wake workspace (clear): %s: %w", strings.TrimSpace(string(out)), err)
	}

	typeCmd := exec.Command("tmux", "send-keys", "-t", name, prompt)
	if out, err := typeCmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux wake workspace (type): %s: %w", strings.TrimSpace(string(out)), err)
	}

	time.Sleep(150 * time.Millisecond)

	submitCmd := exec.Command("tmux", "send-keys", "-t", name, "Enter")
	if out, err := submitCmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux wake workspace: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

func setRemainOnExit(sessionName string) error {
	cmd := exec.Command("tmux", "set-option", "-t", sessionName, "remain-on-exit", "on")
	if out, err := cmd.CombinedOutput(); err != nil {
		_ = exec.Command("tmux", "kill-session", "-t", sessionName).Run()
		return fmt.Errorf("tmux set-option remain-on-exit: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

func envArgs(env map[string]string) []string {
	if len(env) == 0 {
		return nil
	}

	keys := make([]string, 0, len(env))
	for key := range env {
		keys = append(keys, key)
	}
	sort.Strings(keys)

	args := make([]string, 0, len(keys)*2)
	for _, key := range keys {
		args = append(args, "-e", key+"="+env[key])
	}
	return args
}

func commandWithEnv(argv []string, env map[string]string) []string {
	if len(env) == 0 {
		return argv
	}

	args := append([]string{"env"}, envPairs(env)...)
	args = append(args, argv...)
	return args
}

func envPairs(env map[string]string) []string {
	if len(env) == 0 {
		return nil
	}

	keys := make([]string, 0, len(env))
	for key := range env {
		keys = append(keys, key)
	}
	sort.Strings(keys)

	pairs := make([]string, 0, len(keys))
	for _, key := range keys {
		pairs = append(pairs, key+"="+env[key])
	}
	return pairs
}

// IsIdle checks if a workspace's tmux session appears to be at an input prompt
// (i.e., the agent is waiting for user input, not executing tools or generating).
func IsIdle(workspace string) bool {
	name := SessionName(workspace)
	out, err := exec.Command("tmux", "capture-pane", "-t", name, "-p").Output()
	if err != nil {
		return false
	}

	lines := strings.Split(strings.TrimRight(string(out), "\n"), "\n")

	// Find last non-empty line
	lastLine := ""
	for i := len(lines) - 1; i >= 0; i-- {
		trimmed := strings.TrimSpace(lines[i])
		if trimmed != "" {
			lastLine = trimmed
			break
		}
	}
	if lastLine == "" {
		return false
	}

	// Prompt patterns indicating the agent is waiting for input
	idlePatterns := []string{"❯", "> ", "$ ", "# ", "claude>"}
	for _, p := range idlePatterns {
		if strings.HasSuffix(lastLine, p) || lastLine == strings.TrimSpace(p) {
			return true
		}
	}
	// Claude Code shows just ">" or "❯" on the prompt line
	if lastLine == ">" || lastLine == "❯" {
		return true
	}

	return false
}

// SendRawKey sends literal text to a tmux session without appending Enter.
// The -l flag prevents tmux from interpreting key names.
func SendRawKey(sessionName, key string) error {
	cmd := exec.Command("tmux", "send-keys", "-t", sessionName, "-l", key)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux send-keys: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

// SendSpecialKeyToSession sends named keys (Enter, Escape, C-c, etc.)
// to a tmux session by session name (not workspace name).
func SendSpecialKeyToSession(sessionName string, keys ...string) error {
	args := []string{"send-keys", "-t", sessionName}
	args = append(args, keys...)
	cmd := exec.Command("tmux", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux send-keys: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

func isInsideTmux() bool {
	return os.Getenv("TMUX") != ""
}

func encodeWorkspaceName(workspace string) string {
	return strings.ReplaceAll(workspace, ".", "_")
}

func decodeWorkspaceName(name string) string {
	return strings.ReplaceAll(name, "_", ".")
}
