package workspace

import (
	"fmt"
	"os"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
)

var (
	workspaceSessionExists               = tmux.SessionExists
	workspaceCreateSessionWithEnv        = tmux.CreateSessionWithEnv
	workspaceCreateSessionWithCommandEnv = tmux.CreateSessionWithCommandEnv
	workspaceCreateSessionWithArgsEnv    = tmux.CreateSessionWithArgsEnv
	workspaceCreateSessionWithArgs       = tmux.CreateSessionWithArgs
	workspaceDestroySession              = tmux.DestroySession
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

func EnsureArtifacts(name string, ws config.Workspace, socketPath, configPath string) error {
	runtime := agent.NormalizeRuntime(ws.Runtime)
	if _, err := agent.Get(runtime); err != nil {
		return err
	}

	if err := os.MkdirAll(ws.Dir, 0o755); err != nil {
		return fmt.Errorf("create workspace dir: %w", err)
	}
	if err := WriteMCPConfig(ws.Dir, name, daemon.ExpandSocketPath(socketPath), configPath); err != nil {
		return fmt.Errorf("write mcp config: %w", err)
	}
	if err := WriteInstructions(ws.Dir, name, runtime, ws.Instructions); err != nil {
		return fmt.Errorf("write instructions: %w", err)
	}
	if runtime == agent.RuntimeCodex {
		if err := EnsureCodexConfig(ws.Dir, name, socketPath, configPath); err != nil {
			return err
		}
	}
	return nil
}

func (m *Manager) Create(name string, ws config.Workspace) error {
	return m.create(name, ws, false)
}

func (m *Manager) create(name string, ws config.Workspace, fresh bool) error {
	runtime := agent.NormalizeRuntime(ws.Runtime)
	if _, err := agent.Get(runtime); err != nil {
		return err
	}

	if err := EnsureArtifacts(name, ws, m.socketPath, m.configPath); err != nil {
		return err
	}

	// Create tmux session
	if workspaceSessionExists(name) {
		return fmt.Errorf("tmux session %q already exists", tmux.SessionName(name))
	}
	// Use an explicit agent command when configured, otherwise dispatch through ax.
	if ws.Agent != "" {
		if ws.Agent == "none" {
			if err := workspaceCreateSessionWithEnv(name, ws.Dir, ws.Shell, ws.Env); err != nil {
				return fmt.Errorf("create tmux session: %w", err)
			}
			return nil
		}
		if err := workspaceCreateSessionWithCommandEnv(name, ws.Dir, ws.Agent, ws.Env); err != nil {
			return fmt.Errorf("create tmux session: %w", err)
		}
		return nil
	}

	axBin, err := axBinaryPath()
	if err != nil {
		return fmt.Errorf("resolve ax binary: %w", err)
	}
	return workspaceCreateSessionWithArgsEnv(name, ws.Dir, managedRunAgentArgs(axBin, runtime, name, m.socketPath, m.configPath, fresh), ws.Env)
}

func managedRunAgentArgs(axBin, runtime, workspace, socketPath, configPath string, fresh bool) []string {
	args := []string{
		axBin,
		"run-agent",
		"--runtime", runtime,
		"--workspace", workspace,
		"--socket", socketPath,
		"--config", configPath,
	}
	if fresh {
		args = append(args, "--fresh")
	}
	return args
}

func (m *Manager) Restart(name string, ws config.Workspace) error {
	if err := CleanupWorkspaceState(name, ws.Dir); err != nil {
		return fmt.Errorf("reset workspace state: %w", err)
	}
	return m.create(name, ws, true)
}

func (m *Manager) Destroy(name string, dir string) error {
	// Kill tmux session
	if workspaceSessionExists(name) {
		if err := workspaceDestroySession(name); err != nil {
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
		if workspaceSessionExists(name) {
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
		if !workspaceSessionExists(name) {
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
