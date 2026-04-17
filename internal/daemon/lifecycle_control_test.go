package daemon

import (
	"encoding/json"
	"io"
	"log"
	"testing"

	"github.com/ashon/ax/internal/types"
)

func installLifecycleStubs(d *Daemon, start, stop, restart lifecycleControlFunc) {
	d.sessionMgr = newSessionManager(sessionManagerDeps{
		socketPath:    d.socketPath,
		registry:      d.registry,
		queue:         d.queue,
		taskStore:     d.taskStore,
		wakeScheduler: d.wakeScheduler,
		logger:        d.logger,
		startTarget:   start,
		stopTarget:    stop,
		restartTarget: restart,
	})
}

func decodeLifecycleControlResponse(t *testing.T, env *Envelope) ControlLifecycleResponse {
	t.Helper()

	if env.Type != MsgResponse {
		t.Fatalf("expected response, got %s", env.Type)
	}
	var payload ResponsePayload
	if err := env.DecodePayload(&payload); err != nil {
		t.Fatalf("decode response payload: %v", err)
	}
	var result ControlLifecycleResponse
	if err := json.Unmarshal(payload.Data, &result); err != nil {
		t.Fatalf("decode control lifecycle response: %v", err)
	}
	return result
}

func TestHandleEnvelopeRoutesControlLifecycleStart(t *testing.T) {
	d := &Daemon{
		socketPath: "/tmp/daemon.sock",
		logger:     log.New(io.Discard, "", 0),
	}

	var gotSocketPath, gotConfigPath, gotName string
	installLifecycleStubs(d,
		func(socketPath, configPath, target string) (types.LifecycleTarget, error) {
			gotSocketPath = socketPath
			gotConfigPath = configPath
			gotName = target
			return types.LifecycleTarget{
				Name:           target,
				Kind:           types.LifecycleTargetWorkspace,
				ManagedSession: true,
			}, nil
		},
		nil, nil,
	)

	requester := "ax.orchestrator"
	env, err := NewEnvelope("ctl-1", MsgControlLifecycle, &ControlLifecyclePayload{
		ConfigPath: "/tmp/project/.ax/config.yaml",
		Name:       "worker",
		Action:     types.LifecycleActionStart,
	})
	if err != nil {
		t.Fatalf("new envelope: %v", err)
	}

	resp, err := d.handleEnvelope(nil, env, &requester)
	if err != nil {
		t.Fatalf("handle envelope: %v", err)
	}

	if gotSocketPath != d.socketPath || gotConfigPath != "/tmp/project/.ax/config.yaml" || gotName != "worker" {
		t.Fatalf("unexpected call args: socket=%q config=%q name=%q", gotSocketPath, gotConfigPath, gotName)
	}

	result := decodeLifecycleControlResponse(t, resp)
	if result.Action != types.LifecycleActionStart {
		t.Fatalf("action = %q, want start", result.Action)
	}
	if result.Target.Kind != types.LifecycleTargetWorkspace {
		t.Fatalf("target kind = %q, want workspace", result.Target.Kind)
	}
	if !result.Running {
		t.Fatal("expected running=true after start")
	}
}

func TestHandleControlLifecycleRequiresRegistration(t *testing.T) {
	d := &Daemon{socketPath: "/tmp/daemon.sock"}
	requester := ""
	env, err := NewEnvelope("ctl-2", MsgControlLifecycle, &ControlLifecyclePayload{
		ConfigPath: "/tmp/project/.ax/config.yaml",
		Name:       "worker",
		Action:     types.LifecycleActionStop,
	})
	if err != nil {
		t.Fatalf("new envelope: %v", err)
	}

	if _, err := d.handleEnvelope(nil, env, &requester); err == nil || err.Error() != "not registered" {
		t.Fatalf("expected not registered error, got %v", err)
	}
}

func TestHandleControlLifecycleRequiresConfigPath(t *testing.T) {
	d := &Daemon{socketPath: "/tmp/daemon.sock"}
	requester := "ax.orchestrator"
	env, err := NewEnvelope("ctl-3", MsgControlLifecycle, &ControlLifecyclePayload{
		Name:   "worker",
		Action: types.LifecycleActionRestart,
	})
	if err != nil {
		t.Fatalf("new envelope: %v", err)
	}

	if _, err := d.handleEnvelope(nil, env, &requester); err == nil || err.Error() != "config_path is required" {
		t.Fatalf("expected config_path error, got %v", err)
	}
}

func TestHandleControlLifecycleRejectsInvalidAction(t *testing.T) {
	d := &Daemon{socketPath: "/tmp/daemon.sock"}
	requester := "ax.orchestrator"
	env, err := NewEnvelope("ctl-4", MsgControlLifecycle, &ControlLifecyclePayload{
		ConfigPath: "/tmp/project/.ax/config.yaml",
		Name:       "worker",
		Action:     types.LifecycleAction("bounce"),
	})
	if err != nil {
		t.Fatalf("new envelope: %v", err)
	}

	if _, err := d.handleEnvelope(nil, env, &requester); err == nil || err.Error() != `invalid lifecycle action "bounce"` {
		t.Fatalf("expected invalid action error, got %v", err)
	}
}

func TestHandleControlLifecyclePropagatesLifecycleErrors(t *testing.T) {
	d := &Daemon{socketPath: "/tmp/daemon.sock"}
	installLifecycleStubs(d,
		nil,
		func(socketPath, configPath, target string) (types.LifecycleTarget, error) {
			return types.LifecycleTarget{}, assertErr(`orchestrator "orchestrator" does not support targeted stop because it is not a managed session`)
		},
		nil,
	)

	requester := "ax.orchestrator"
	env, err := NewEnvelope("ctl-5", MsgControlLifecycle, &ControlLifecyclePayload{
		ConfigPath: "/tmp/project/.ax/config.yaml",
		Name:       "orchestrator",
		Action:     types.LifecycleActionStop,
	})
	if err != nil {
		t.Fatalf("new envelope: %v", err)
	}

	if _, err := d.handleEnvelope(nil, env, &requester); err == nil || err.Error() != `orchestrator "orchestrator" does not support targeted stop because it is not a managed session` {
		t.Fatalf("expected propagated lifecycle error, got %v", err)
	}
}

type assertErr string

func (e assertErr) Error() string {
	return string(e)
}
