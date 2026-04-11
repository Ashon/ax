package tmux

import (
	"fmt"
	"os"
	"os/exec"
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
	name := SessionName(workspace)

	args := []string{"new-session", "-d", "-s", name, "-c", dir}
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
	name := SessionName(workspace)

	// Use "sh -c 'exec <command>'" so the command replaces the shell process.
	// remain-on-exit keeps the pane open if the command exits, allowing restart.
	cmd := exec.Command("tmux", "new-session", "-d", "-s", name, "-c", dir,
		"sh", "-c", command)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux new-session: %s: %w", strings.TrimSpace(string(out)), err)
	}

	// Set remain-on-exit so session doesn't vanish if claude exits
	exec.Command("tmux", "set-option", "-t", name, "remain-on-exit", "on").Run()

	return nil
}

func CreateSessionWithArgs(workspace, dir string, argv []string) error {
	name := SessionName(workspace)

	args := []string{"new-session", "-d", "-s", name, "-c", dir}
	args = append(args, argv...)

	cmd := exec.Command("tmux", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux new-session: %s: %w", strings.TrimSpace(string(out)), err)
	}

	exec.Command("tmux", "set-option", "-t", name, "remain-on-exit", "on").Run()
	return nil
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
	out, err := cmd.Output()
	if err != nil {
		// No server running = no sessions
		if strings.Contains(string(out), "no server running") || strings.Contains(err.Error(), "exit status") {
			return nil, nil
		}
		return nil, fmt.Errorf("tmux list-sessions: %w", err)
	}

	var sessions []SessionInfo
	for _, line := range strings.Split(strings.TrimSpace(string(out)), "\n") {
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

func SendKeys(workspace, keys string) error {
	name := SessionName(workspace)
	cmd := exec.Command("tmux", "send-keys", "-t", name, keys, "Enter")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("tmux send-keys: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
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
	return SendSpecialKeys(workspace, "Escape")
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
