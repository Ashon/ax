package workspace

import (
	"fmt"
	"os"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
)

type Manager struct {
	socketPath string
	configPath string
}

func NewManager(socketPath, configPath string) *Manager {
	return &Manager{
		socketPath: daemon.ExpandSocketPath(socketPath),
		configPath: configPath,
	}
}

func (m *Manager) Create(name string, ws config.Workspace) error {
	runtime := agent.NormalizeRuntime(ws.Runtime)
	if _, err := agent.Get(runtime); err != nil {
		return err
	}

	// Ensure directory exists
	if err := os.MkdirAll(ws.Dir, 0o755); err != nil {
		return fmt.Errorf("create workspace dir: %w", err)
	}

	// Write .mcp.json
	if err := WriteMCPConfig(ws.Dir, name, m.socketPath, m.configPath); err != nil {
		return fmt.Errorf("write mcp config: %w", err)
	}

	// Write CLAUDE.md with instructions
	if ws.Instructions != "" {
		if err := WriteInstructions(ws.Dir, name, runtime, ws.Instructions); err != nil {
			return fmt.Errorf("write instructions: %w", err)
		}
	}

	// Create tmux session
	if tmux.SessionExists(name) {
		return fmt.Errorf("tmux session %q already exists", tmux.SessionName(name))
	}
	// Use an explicit agent command when configured, otherwise dispatch through ax.
	if ws.Agent != "" {
		if ws.Agent == "none" {
			if err := tmux.CreateSession(name, ws.Dir, ws.Shell); err != nil {
				return fmt.Errorf("create tmux session: %w", err)
			}
			return nil
		}
		if err := tmux.CreateSessionWithCommand(name, ws.Dir, ws.Agent); err != nil {
			return fmt.Errorf("create tmux session: %w", err)
		}
		return nil
	}

	axBin, err := axBinaryPath()
	if err != nil {
		return fmt.Errorf("resolve ax binary: %w", err)
	}
	return tmux.CreateSessionWithArgs(name, ws.Dir, []string{
		axBin,
		"run-agent",
		"--runtime", runtime,
		"--workspace", name,
		"--socket", m.socketPath,
		"--config", m.configPath,
	})
}

func (m *Manager) Destroy(name string, dir string) error {
	// Kill tmux session
	if tmux.SessionExists(name) {
		if err := tmux.DestroySession(name); err != nil {
			return fmt.Errorf("destroy tmux session: %w", err)
		}
	}

	// Remove .mcp.json entry and ax instructions
	if dir != "" {
		RemoveMCPConfig(dir)
		RemoveInstructions(dir)
	}

	return nil
}

func (m *Manager) CreateAll(cfg *config.Config) error {
	for name, ws := range cfg.Workspaces {
		if tmux.SessionExists(name) {
			fmt.Printf("  %s: already running (skipped)\n", name)
			continue
		}
		if err := m.Create(name, ws); err != nil {
			return fmt.Errorf("create workspace %q: %w", name, err)
		}
		fmt.Printf("  %s: started (dir: %s)\n", name, ws.Dir)
	}
	return nil
}

func (m *Manager) DestroyAll(cfg *config.Config) error {
	for name, ws := range cfg.Workspaces {
		if !tmux.SessionExists(name) {
			fmt.Printf("  %s: not running (skipped)\n", name)
			continue
		}
		if err := m.Destroy(name, ws.Dir); err != nil {
			return fmt.Errorf("destroy workspace %q: %w", name, err)
		}
		fmt.Printf("  %s: stopped\n", name)
	}
	return nil
}
