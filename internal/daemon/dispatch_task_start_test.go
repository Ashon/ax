package daemon

import (
	"encoding/json"
	"io"
	"log"
	"net"
	"testing"
)

func TestHandleStartTaskEnvelopeDispatchesViaDaemonWithStoredConfigPath(t *testing.T) {
	stateDir := t.TempDir()
	d := &Daemon{
		socketPath:    "/tmp/ax.sock",
		queue:         NewMessageQueue(),
		history:       NewHistory(stateDir, 50),
		registry:      NewRegistry(),
		taskStore:     NewTaskStore(stateDir),
		wakeScheduler: NewWakeScheduler(NewMessageQueue(), nil),
		logger:        log.New(io.Discard, "", 0),
	}

	dispatched := false
	d.sessionMgr = newSessionManager(sessionManagerDeps{
		socketPath:    d.socketPath,
		registry:      d.registry,
		queue:         d.queue,
		taskStore:     d.taskStore,
		wakeScheduler: d.wakeScheduler,
		logger:        d.logger,
		dispatchRunnable: func(socketPath, configPath, target, sender string, fresh bool) error {
			dispatched = true
			if socketPath != "/tmp/ax.sock" {
				t.Fatalf("unexpected socket path %q", socketPath)
			}
			if configPath != "/tmp/project/.ax/config.yaml" {
				t.Fatalf("unexpected config path %q", configPath)
			}
			if target != "worker" {
				t.Fatalf("unexpected target %q", target)
			}
			if sender != "orchestrator" {
				t.Fatalf("unexpected sender %q", sender)
			}
			if fresh {
				t.Fatal("unexpected fresh dispatch")
			}
			return nil
		},
	})

	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()
	d.registry.Register("orchestrator", "", "", "/tmp/project/.ax/config.yaml", 0, clientConn)

	env, _ := NewEnvelope("start-task", MsgStartTask, &StartTaskPayload{
		Title:    "daemon dispatch",
		Assignee: "worker",
		Message:  "Inspect the daemon-side dispatch path",
	})
	resp, err := d.handleStartTaskEnvelope(env, "orchestrator")
	if err != nil {
		t.Fatalf("handle start_task: %v", err)
	}

	var payload ResponsePayload
	if err := resp.DecodePayload(&payload); err != nil {
		t.Fatalf("decode response payload: %v", err)
	}
	var started StartTaskResponse
	if err := json.Unmarshal(payload.Data, &started); err != nil {
		t.Fatalf("unmarshal start task response: %v", err)
	}
	if started.Dispatch.Status != "queued" {
		t.Fatalf("dispatch status = %q, want queued", started.Dispatch.Status)
	}
	if started.Task.DispatchConfigPath != "/tmp/project/.ax/config.yaml" {
		t.Fatalf("dispatch config path = %q", started.Task.DispatchConfigPath)
	}
	if !dispatched {
		t.Fatal("expected daemon to dispatch queued task")
	}
}
