package workspace

import (
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/types"
)

type LifecycleAction = types.LifecycleAction

const (
	LifecycleActionStart   = types.LifecycleActionStart
	LifecycleActionStop    = types.LifecycleActionStop
	LifecycleActionRestart = types.LifecycleActionRestart
)

type LifecycleTargetKind = types.LifecycleTargetKind

const (
	LifecycleTargetWorkspace    = types.LifecycleTargetWorkspace
	LifecycleTargetOrchestrator = types.LifecycleTargetOrchestrator
)

type LifecycleTarget = types.LifecycleTarget

type resolvedLifecycleTarget struct {
	target       types.LifecycleTarget
	workspace    DesiredWorkspace
	orchestrator DesiredOrchestrator
}

// StartNamedTarget starts a configured workspace or managed child orchestrator
// by exact name. Unmanaged targets such as the root orchestrator are rejected.
func StartNamedTarget(socketPath, configPath, target string) (types.LifecycleTarget, error) {
	return controlNamedTarget(socketPath, configPath, target, types.LifecycleActionStart)
}

// StopNamedTarget stops a running workspace or managed child orchestrator by
// exact name without deleting its generated artifacts.
func StopNamedTarget(socketPath, configPath, target string) (types.LifecycleTarget, error) {
	return controlNamedTarget(socketPath, configPath, target, types.LifecycleActionStop)
}

// RestartNamedTarget recycles a configured workspace or managed child
// orchestrator by exact name. Restart performs a real cleanup/recreate cycle.
func RestartNamedTarget(socketPath, configPath, target string) (types.LifecycleTarget, error) {
	return controlNamedTarget(socketPath, configPath, target, types.LifecycleActionRestart)
}

func controlNamedTarget(socketPath, configPath, targetName string, action types.LifecycleAction) (types.LifecycleTarget, error) {
	target, err := resolveLifecycleTarget(socketPath, configPath, targetName)
	if err != nil {
		return types.LifecycleTarget{}, err
	}

	switch target.target.Kind {
	case types.LifecycleTargetWorkspace:
		if err := controlWorkspaceTarget(socketPath, configPath, target, action); err != nil {
			return types.LifecycleTarget{}, err
		}
	case types.LifecycleTargetOrchestrator:
		if err := controlOrchestratorTarget(socketPath, configPath, target, action); err != nil {
			return types.LifecycleTarget{}, err
		}
	default:
		return types.LifecycleTarget{}, fmt.Errorf("unsupported target kind %q", target.target.Kind)
	}

	return target.target, nil
}

func resolveLifecycleTarget(socketPath, configPath, targetName string) (resolvedLifecycleTarget, error) {
	targetName = strings.TrimSpace(targetName)
	if targetName == "" {
		return resolvedLifecycleTarget{}, fmt.Errorf("target name is required")
	}

	desired, err := loadDispatchDesiredState(socketPath, configPath)
	if err != nil {
		return resolvedLifecycleTarget{}, err
	}

	workspaceEntry, hasWorkspace := desired.Workspaces[targetName]
	orchestratorEntry, hasOrchestrator := desired.Orchestrators[targetName]

	switch {
	case hasWorkspace && hasOrchestrator:
		return resolvedLifecycleTarget{}, fmt.Errorf("target %q is ambiguous in %s because it matches both a workspace and an orchestrator", targetName, cleanPath(configPath))
	case hasWorkspace:
		return resolvedLifecycleTarget{
			target: types.LifecycleTarget{
				Name:           workspaceEntry.Name,
				Kind:           types.LifecycleTargetWorkspace,
				ManagedSession: true,
			},
			workspace: workspaceEntry,
		}, nil
	case hasOrchestrator:
		return resolvedLifecycleTarget{
			target: types.LifecycleTarget{
				Name:           orchestratorEntry.Name,
				Kind:           types.LifecycleTargetOrchestrator,
				ManagedSession: orchestratorEntry.ManagedSession,
			},
			orchestrator: orchestratorEntry,
		}, nil
	default:
		return resolvedLifecycleTarget{}, fmt.Errorf("target %q is not defined in %s", targetName, cleanPath(configPath))
	}
}

func controlWorkspaceTarget(socketPath, configPath string, target resolvedLifecycleTarget, action types.LifecycleAction) error {
	manager := NewManager(socketPath, configPath)

	switch action {
	case types.LifecycleActionStart:
		if workspaceSessionExists(target.target.Name) {
			return alreadyRunningError(target.target)
		}
		return manager.Create(target.target.Name, target.workspace.Workspace)
	case types.LifecycleActionStop:
		return stopSessionTarget(target.target)
	case types.LifecycleActionRestart:
		return manager.Restart(target.target.Name, target.workspace.Workspace)
	default:
		return fmt.Errorf("unsupported lifecycle action %q", action)
	}
}

func controlOrchestratorTarget(socketPath, configPath string, target resolvedLifecycleTarget, action types.LifecycleAction) error {
	if !target.target.ManagedSession {
		return unsupportedManagedSessionError(target.target, action)
	}

	switch action {
	case types.LifecycleActionStart:
		if workspaceSessionExists(target.target.Name) {
			return alreadyRunningError(target.target)
		}
		return EnsureOrchestrator(target.orchestrator.Node, target.orchestrator.ParentName, daemonutil.ExpandSocketPath(socketPath), configPath, true)
	case types.LifecycleActionStop:
		return stopSessionTarget(target.target)
	case types.LifecycleActionRestart:
		if err := CleanupOrchestratorState(target.target.Name, target.orchestrator.ArtifactDir); err != nil {
			return err
		}
		return EnsureOrchestrator(target.orchestrator.Node, target.orchestrator.ParentName, daemonutil.ExpandSocketPath(socketPath), configPath, true)
	default:
		return fmt.Errorf("unsupported lifecycle action %q", action)
	}
}

func stopSessionTarget(target types.LifecycleTarget) error {
	if !workspaceSessionExists(target.Name) {
		return notRunningError(target)
	}
	if err := workspaceDestroySession(target.Name); err != nil {
		return fmt.Errorf("destroy tmux session: %w", err)
	}
	return nil
}

func alreadyRunningError(target types.LifecycleTarget) error {
	return fmt.Errorf("%s %q is already running", target.Kind, target.Name)
}

func notRunningError(target types.LifecycleTarget) error {
	return fmt.Errorf("%s %q is not running", target.Kind, target.Name)
}

func unsupportedManagedSessionError(target types.LifecycleTarget, action types.LifecycleAction) error {
	return fmt.Errorf("%s %q does not support targeted %s because it is not a managed session", target.Kind, target.Name, action)
}
