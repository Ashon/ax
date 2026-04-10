package workspace

import (
	"fmt"
	"os"

	"github.com/ashon/amux/internal/config"
	"github.com/ashon/amux/internal/daemon"
	"github.com/ashon/amux/internal/tmux"
)

type Manager struct {
	socketPath string
}

func NewManager(socketPath string) *Manager {
	return &Manager{
		socketPath: daemon.ExpandSocketPath(socketPath),
	}
}

func (m *Manager) Create(name string, ws config.Workspace) error {
	// Ensure directory exists
	if err := os.MkdirAll(ws.Dir, 0o755); err != nil {
		return fmt.Errorf("create workspace dir: %w", err)
	}

	// Write .mcp.json
	if err := WriteMCPConfig(ws.Dir, name, m.socketPath); err != nil {
		return fmt.Errorf("write mcp config: %w", err)
	}

	// Write CLAUDE.md with instructions
	if ws.Instructions != "" {
		if err := WriteInstructions(ws.Dir, name, ws.Instructions); err != nil {
			return fmt.Errorf("write instructions: %w", err)
		}
	}

	// Create tmux session
	if tmux.SessionExists(name) {
		return fmt.Errorf("tmux session %q already exists", tmux.SessionName(name))
	}
	if err := tmux.CreateSession(name, ws.Dir, ws.Shell); err != nil {
		return fmt.Errorf("create tmux session: %w", err)
	}

	// Auto-start agent (default: claude --dangerously-skip-permissions)
	agent := ws.Agent
	if agent == "" {
		agent = "claude --dangerously-skip-permissions"
	}
	if agent != "none" {
		tmux.SendKeys(name, agent)
	}

	return nil
}

func (m *Manager) Destroy(name string, dir string) error {
	// Kill tmux session
	if tmux.SessionExists(name) {
		if err := tmux.DestroySession(name); err != nil {
			return fmt.Errorf("destroy tmux session: %w", err)
		}
	}

	// Remove .mcp.json entry and amux instructions
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
