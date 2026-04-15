package workspace

import (
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/tmux"
)

var workspaceWakeSession = tmux.WakeWorkspace

// DispatchRunnableWork ensures the target session is ready to receive work and
// then injects the standard wake prompt that tells the agent to process queued
// messages.
func DispatchRunnableWork(socketPath, configPath, target, sender string, fresh bool) error {
	target = strings.TrimSpace(target)
	if target == "" {
		return fmt.Errorf("dispatch target is required")
	}
	sender = strings.TrimSpace(sender)
	if sender == "" {
		return fmt.Errorf("dispatch sender is required")
	}

	if err := EnsureDispatchTarget(socketPath, configPath, target, fresh); err != nil {
		return err
	}
	if err := workspaceWakeSession(target, daemonutil.WakePrompt(sender, fresh)); err != nil {
		return fmt.Errorf("wake %q: %w", target, err)
	}
	return nil
}

// EnsureDispatchTarget makes sure the named target session exists before a
// queued task/message is woken. When fresh is true the managed target is
// recreated so the next dispatch starts from a clean session.
func EnsureDispatchTarget(socketPath, configPath, target string, fresh bool) error {
	target = strings.TrimSpace(target)
	if target == "" {
		return fmt.Errorf("dispatch target is required")
	}

	if !fresh && workspaceSessionExists(target) {
		return nil
	}

	desired, err := loadDispatchDesiredState(socketPath, configPath)
	if err != nil {
		return err
	}

	if entry, ok := desired.Workspaces[target]; ok {
		return ensureWorkspaceDispatchTarget(socketPath, configPath, entry, fresh)
	}
	if entry, ok := desired.Orchestrators[target]; ok {
		return ensureOrchestratorDispatchTarget(socketPath, configPath, entry, fresh)
	}

	if !fresh && workspaceSessionExists(target) {
		return nil
	}
	return fmt.Errorf("dispatch target %q is not defined in %s", target, cleanPath(configPath))
}

func loadDispatchDesiredState(socketPath, configPath string) (*DesiredState, error) {
	cfg, err := config.Load(configPath)
	if err != nil {
		return nil, fmt.Errorf("load config: %w", err)
	}

	tree, err := config.LoadTree(configPath)
	if err != nil {
		return nil, fmt.Errorf("load config tree: %w", err)
	}

	includeRoot := tree == nil || !tree.DisableRootOrchestrator
	desired, err := BuildDesiredState(cfg, tree, socketPath, configPath, includeRoot)
	if err != nil {
		return nil, fmt.Errorf("build desired dispatch state: %w", err)
	}
	return desired, nil
}

func ensureWorkspaceDispatchTarget(socketPath, configPath string, entry DesiredWorkspace, fresh bool) error {
	manager := NewManager(socketPath, configPath)
	if fresh {
		return manager.Restart(entry.Name, entry.Workspace)
	}
	if workspaceSessionExists(entry.Name) {
		return nil
	}
	return manager.Create(entry.Name, entry.Workspace)
}

func ensureOrchestratorDispatchTarget(socketPath, configPath string, entry DesiredOrchestrator, fresh bool) error {
	if entry.Node == nil {
		return fmt.Errorf("orchestrator %q is missing project metadata", entry.Name)
	}
	if !entry.ManagedSession {
		if fresh {
			return fmt.Errorf("orchestrator %q does not support fresh restart because it is not a managed session", entry.Name)
		}
		if workspaceSessionExists(entry.Name) {
			return nil
		}
		return fmt.Errorf("orchestrator %q is not running and is not a managed session", entry.Name)
	}

	if fresh {
		if err := CleanupOrchestratorState(entry.Name, entry.ArtifactDir); err != nil {
			return err
		}
	}
	if workspaceSessionExists(entry.Name) {
		return nil
	}
	return EnsureOrchestrator(entry.Node, entry.ParentName, daemonutil.ExpandSocketPath(socketPath), configPath, true)
}
