package daemon

import (
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/workspace"
)

// agentWorkspaceManager is the subset of workspace.Manager used for agent
// lifecycle transitions. Exposed as an interface so tests can inject fakes.
type agentWorkspaceManager interface {
	Create(name string, ws config.Workspace) error
	Restart(name string, ws config.Workspace) error
	Destroy(name, dir string) error
}

// agentLifecycleOps bundles the workspace-layer operations used by the
// agent_lifecycle handler so tests can substitute fakes without mutating
// package-level globals.
type agentLifecycleOps struct {
	newManager          func(socketPath, configPath string) agentWorkspaceManager
	ensureOrchestrator  func(node *config.ProjectNode, parentName, socketPath, configPath string, startSession bool) error
	cleanupOrchestrator func(name, artifactDir string) error
	sessionExists       func(name string) bool
}

func defaultAgentLifecycleOps() *agentLifecycleOps {
	return &agentLifecycleOps{
		newManager: func(socketPath, configPath string) agentWorkspaceManager {
			return workspace.NewManager(socketPath, configPath)
		},
		ensureOrchestrator:  workspace.EnsureOrchestrator,
		cleanupOrchestrator: workspace.CleanupOrchestratorState,
		sessionExists:       tmux.SessionExists,
	}
}

type agentLifecycleTarget struct {
	Name           string
	Kind           string
	ManagedSession bool
	Workspace      *config.Workspace
	Orchestrator   *workspace.DesiredOrchestrator
	Limit          string
}

func (d *Daemon) handleAgentLifecycleEnvelope(env *Envelope, requester string) (*Envelope, error) {
	var p AgentLifecyclePayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode agent_lifecycle: %w", err)
	}
	if err := requireRegisteredWorkspace(requester); err != nil {
		return nil, err
	}

	configPath := strings.TrimSpace(p.ConfigPath)
	if configPath == "" {
		return nil, fmt.Errorf("config_path is required")
	}

	action, err := parseLifecycleAction(p.Action)
	if err != nil {
		return nil, err
	}

	target, err := d.resolveAgentLifecycleTarget(configPath, p.Name)
	if err != nil {
		return nil, err
	}
	result, err := d.applyAgentLifecycleAction(configPath, target, action)
	if err != nil {
		return nil, err
	}
	d.registry.Touch(requester)
	return NewResponseEnvelope(env.ID, result)
}

func (d *Daemon) resolveAgentLifecycleTarget(configPath, name string) (agentLifecycleTarget, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return agentLifecycleTarget{}, fmt.Errorf("name is required")
	}

	cfg, err := config.Load(configPath)
	if err != nil {
		return agentLifecycleTarget{}, fmt.Errorf("load ax config: %w", err)
	}

	tree, err := config.LoadTree(configPath)
	if err != nil {
		return agentLifecycleTarget{}, fmt.Errorf("load config tree: %w", err)
	}
	includeRoot := tree == nil || !tree.DisableRootOrchestrator
	desired, err := workspace.BuildDesiredState(cfg, tree, d.socketPath, configPath, includeRoot)
	if err != nil {
		return agentLifecycleTarget{}, fmt.Errorf("build desired state: %w", err)
	}

	if entry, ok := desired.Workspaces[name]; ok {
		ws := entry.Workspace
		return agentLifecycleTarget{
			Name:           name,
			Kind:           "workspace",
			ManagedSession: true,
			Workspace:      &ws,
		}, nil
	}

	if entry, ok := desired.Orchestrators[name]; ok {
		target := agentLifecycleTarget{
			Name:           name,
			Kind:           "orchestrator",
			ManagedSession: entry.ManagedSession,
			Orchestrator:   &entry,
		}
		if entry.Root || !entry.ManagedSession {
			target.Limit = "root orchestrator lifecycle is not supported here because it is not a daemon-managed session"
		}
		return target, nil
	}

	return agentLifecycleTarget{}, fmt.Errorf("Agent %q is not defined exactly in %s; use list_agents for exact configured names", name, configPath)
}

func (d *Daemon) applyAgentLifecycleAction(configPath string, target agentLifecycleTarget, action types.LifecycleAction) (*AgentLifecycleResponse, error) {
	if strings.TrimSpace(target.Limit) != "" {
		return nil, fmt.Errorf("Agent %q does not support %s: %s", target.Name, action, target.Limit)
	}

	ops := d.agentOps
	if ops == nil {
		ops = defaultAgentLifecycleOps()
	}

	existedBefore := ops.sessionExists(target.Name)
	result := &AgentLifecycleResponse{
		Name:                target.Name,
		Action:              string(action),
		TargetKind:          target.Kind,
		ManagedSession:      target.ManagedSession,
		ExactMatch:          true,
		SessionExistsBefore: existedBefore,
	}

	switch target.Kind {
	case "workspace":
		if target.Workspace == nil {
			return nil, fmt.Errorf("workspace target %q is missing configuration", target.Name)
		}
		manager := ops.newManager(d.socketPath, configPath)
		switch action {
		case types.LifecycleActionStart:
			if existedBefore {
				result.Status = "already_running"
				break
			}
			if err := manager.Create(target.Name, *target.Workspace); err != nil {
				return nil, fmt.Errorf("start workspace %q: %w", target.Name, err)
			}
			result.Status = "started"
		case types.LifecycleActionStop:
			if err := manager.Destroy(target.Name, target.Workspace.Dir); err != nil {
				return nil, fmt.Errorf("stop workspace %q: %w", target.Name, err)
			}
			if existedBefore {
				result.Status = "stopped"
			} else {
				result.Status = "already_stopped"
			}
		case types.LifecycleActionRestart:
			if err := manager.Restart(target.Name, *target.Workspace); err != nil {
				return nil, fmt.Errorf("restart workspace %q: %w", target.Name, err)
			}
			result.Status = "restarted"
		default:
			return nil, fmt.Errorf("unsupported lifecycle action %q", action)
		}
	case "orchestrator":
		if target.Orchestrator == nil || target.Orchestrator.Node == nil {
			return nil, fmt.Errorf("orchestrator target %q is missing project metadata", target.Name)
		}
		switch action {
		case types.LifecycleActionStart:
			if existedBefore {
				result.Status = "already_running"
				break
			}
			if err := ops.ensureOrchestrator(target.Orchestrator.Node, target.Orchestrator.ParentName, d.socketPath, configPath, true); err != nil {
				return nil, fmt.Errorf("start orchestrator %q: %w", target.Name, err)
			}
			result.Status = "started"
		case types.LifecycleActionStop:
			if err := ops.cleanupOrchestrator(target.Name, target.Orchestrator.ArtifactDir); err != nil {
				return nil, fmt.Errorf("stop orchestrator %q: %w", target.Name, err)
			}
			if existedBefore {
				result.Status = "stopped"
			} else {
				result.Status = "already_stopped"
			}
		case types.LifecycleActionRestart:
			if err := ops.cleanupOrchestrator(target.Name, target.Orchestrator.ArtifactDir); err != nil {
				return nil, fmt.Errorf("restart orchestrator %q: %w", target.Name, err)
			}
			if err := ops.ensureOrchestrator(target.Orchestrator.Node, target.Orchestrator.ParentName, d.socketPath, configPath, true); err != nil {
				return nil, fmt.Errorf("restart orchestrator %q: %w", target.Name, err)
			}
			result.Status = "restarted"
		default:
			return nil, fmt.Errorf("unsupported lifecycle action %q", action)
		}
	default:
		return nil, fmt.Errorf("unsupported lifecycle target kind %q", target.Kind)
	}

	result.SessionExistsAfter = ops.sessionExists(target.Name)
	switch action {
	case types.LifecycleActionStart, types.LifecycleActionRestart:
		if !result.SessionExistsAfter {
			return nil, fmt.Errorf("%s %q completed without leaving a running session", action, target.Name)
		}
	case types.LifecycleActionStop:
		if result.SessionExistsAfter {
			return nil, fmt.Errorf("stop %q completed but the session is still running", target.Name)
		}
	}

	return result, nil
}
