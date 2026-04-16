package daemon

import (
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/workspace"
)

var (
	controlStartNamedTarget   = workspace.StartNamedTarget
	controlStopNamedTarget    = workspace.StopNamedTarget
	controlRestartNamedTarget = workspace.RestartNamedTarget
)

func parseLifecycleAction(value types.LifecycleAction) (types.LifecycleAction, error) {
	action := types.LifecycleAction(strings.TrimSpace(string(value)))
	switch action {
	case types.LifecycleActionStart, types.LifecycleActionStop, types.LifecycleActionRestart:
		return action, nil
	default:
		return "", fmt.Errorf("invalid lifecycle action %q", value)
	}
}

func (d *Daemon) handleControlLifecycleEnvelope(env *Envelope, requester string) (*Envelope, error) {
	var p ControlLifecyclePayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode control_lifecycle: %w", err)
	}
	if err := requireRegisteredWorkspace(requester); err != nil {
		return nil, err
	}

	configPath := strings.TrimSpace(p.ConfigPath)
	if configPath == "" {
		return nil, fmt.Errorf("config_path is required")
	}
	targetName := strings.TrimSpace(p.Name)
	if targetName == "" {
		return nil, fmt.Errorf("name is required")
	}
	action, err := parseLifecycleAction(p.Action)
	if err != nil {
		return nil, err
	}

	target, err := d.sessionManager().control(configPath, targetName, action)
	if err != nil {
		return nil, err
	}
	d.registry.Touch(requester)

	running := action != types.LifecycleActionStop
	if d.logger != nil {
		d.logger.Printf("lifecycle control: requester=%s action=%s target=%s kind=%s running=%t", requester, action, target.Name, target.Kind, running)
	}
	return NewResponseEnvelope(env.ID, &ControlLifecycleResponse{
		Target:  target,
		Action:  action,
		Running: running,
	})
}
