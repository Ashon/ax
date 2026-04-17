package daemon

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/types"
)

func TestNormalizeTaskDispatchBody_RejectsEmbeddedTaskID(t *testing.T) {
	_, err := normalizeTaskDispatchBody("Task ID: 33333333-3333-3333-3333-333333333333\n\nPlease handle this")
	if err == nil {
		t.Fatal("expected embedded Task ID to be rejected")
	}
	if !strings.Contains(err.Error(), "start_task injects the new task ID automatically") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestNormalizeTaskDispatchBody_RejectsBlankMessage(t *testing.T) {
	_, err := normalizeTaskDispatchBody(" \n\t ")
	if err == nil {
		t.Fatal("expected blank message to be rejected")
	}
	if !strings.Contains(err.Error(), "message is required") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func decodeStartTaskResponse(t *testing.T, env *Envelope) StartTaskResponse {
	t.Helper()
	var payload ResponsePayload
	if err := env.DecodePayload(&payload); err != nil {
		t.Fatalf("decode response payload: %v", err)
	}
	var started StartTaskResponse
	if err := json.Unmarshal(payload.Data, &started); err != nil {
		t.Fatalf("unmarshal start task response: %v", err)
	}
	return started
}

func TestHandleStartTask_DispatchesViaDaemonWithStoredConfigPath(t *testing.T) {
	dispatched := false
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
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
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")

	env, _ := NewEnvelope("start-task", MsgStartTask, &StartTaskPayload{
		Title:    "daemon dispatch",
		Assignee: "worker",
		Message:  "Inspect the daemon-side dispatch path",
	})
	resp, err := td.handleStartTaskEnvelope(env, "orchestrator")
	if err != nil {
		t.Fatalf("handle start_task: %v", err)
	}

	started := decodeStartTaskResponse(t, resp)
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

func TestHandleStartTask_FreshModeDispatchesFresh(t *testing.T) {
	var gotFresh bool
	var calls int
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		calls++
		gotFresh = fresh
		return nil
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")

	env, _ := NewEnvelope("start-task-fresh", MsgStartTask, &StartTaskPayload{
		Title:     "fresh start",
		Assignee:  "worker",
		Message:   "Boot the worker from scratch",
		StartMode: string(types.TaskStartFresh),
	})
	if _, err := td.handleStartTaskEnvelope(env, "orchestrator"); err != nil {
		t.Fatalf("handle start_task: %v", err)
	}
	if calls != 1 {
		t.Fatalf("dispatch called %d times, want 1", calls)
	}
	if !gotFresh {
		t.Fatal("expected fresh=true dispatch for StartMode=fresh")
	}
}

func TestHandleStartTask_EmptyMessageWaitsForInput(t *testing.T) {
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		t.Fatal("dispatch must not run when task has no body")
		return nil
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")

	// start_task requires a non-empty message. Exercise waiting_for_input via
	// the lower-level dispatch path with a task that has no DispatchMessage.
	task, err := td.taskStore.CreateWithWorkflow("placeholder", "", "worker", "orchestrator", "", types.TaskStartDefault, types.TaskWorkflowParallel, types.TaskPriorityNormal, 0, "", "/tmp/project/.ax/config.yaml")
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
	dispatch, err := td.dispatchTaskStart(*task)
	if err != nil {
		t.Fatalf("dispatch: %v", err)
	}
	if dispatch.Status != "waiting_for_input" {
		t.Fatalf("dispatch status = %q, want waiting_for_input", dispatch.Status)
	}
	if td.queue.PendingCount("worker") != 0 {
		t.Fatal("queue must stay empty when task waits for input")
	}
}

func TestHandleStartTask_DispatchErrorPreservesEnqueuedMessage(t *testing.T) {
	dispatchErr := errors.New("workspace create failed")
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		return dispatchErr
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")

	env, _ := NewEnvelope("start-task-err", MsgStartTask, &StartTaskPayload{
		Title:    "explodes after enqueue",
		Assignee: "worker",
		Message:  "Partial failure contract",
	})
	_, err := td.handleStartTaskEnvelope(env, "orchestrator")
	if err == nil {
		t.Fatal("expected dispatch error to propagate")
	}
	if !errors.Is(err, dispatchErr) {
		t.Fatalf("expected wrapped dispatchErr, got %v", err)
	}

	// Even after the dispatch error, the message must stay enqueued and the
	// wake scheduler must have been notified -- this is the documented partial
	// failure contract so downstream retries can resume.
	if td.queue.PendingCount("worker") != 1 {
		t.Fatalf("queue pending for worker = %d, want 1 after dispatch error", td.queue.PendingCount("worker"))
	}
	if _, ok := td.wakeScheduler.State("worker"); !ok {
		t.Fatal("expected wake scheduler to retain a pending entry for worker")
	}
	tasks := td.taskStore.List("worker", "", nil)
	if len(tasks) != 1 {
		t.Fatalf("taskStore entries for worker = %d, want 1", len(tasks))
	}
}
