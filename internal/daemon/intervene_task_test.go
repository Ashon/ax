package daemon

import (
	"encoding/json"
	"testing"

	"github.com/ashon/ax/internal/types"
)

func decodeInterveneResponse(t *testing.T, env *Envelope) InterveneTaskResponse {
	t.Helper()
	var payload ResponsePayload
	if err := env.DecodePayload(&payload); err != nil {
		t.Fatalf("decode response payload: %v", err)
	}
	var result InterveneTaskResponse
	if err := json.Unmarshal(payload.Data, &result); err != nil {
		t.Fatalf("decode intervene response: %v", err)
	}
	return result
}

func TestHandleInterveneWake_UsesEnsureRunnableWhenDispatchConfigPresent(t *testing.T) {
	var gotTarget, gotSender, gotConfig string
	var calls int
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		calls++
		gotTarget, gotSender, gotConfig = target, sender, configPath
		if fresh {
			t.Fatal("wake intervene must not request fresh dispatch")
		}
		return nil
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")

	task, err := td.taskStore.CreateWithWorkflow("stuck worker", "", "worker", "orchestrator", "", types.TaskStartDefault, types.TaskWorkflowParallel, types.TaskPriorityNormal, 0, "poke", "/tmp/project/.ax/config.yaml")
	if err != nil {
		t.Fatalf("create task: %v", err)
	}

	env, _ := NewEnvelope("intervene-wake", MsgInterveneTask, &InterveneTaskPayload{
		ID:     task.ID,
		Action: "wake",
	})
	resp, err := td.handleInterveneTaskEnvelope(env, "orchestrator")
	if err != nil {
		t.Fatalf("handle intervene_task wake: %v", err)
	}
	if calls != 1 {
		t.Fatalf("dispatch called %d times, want 1", calls)
	}
	if gotTarget != "worker" || gotSender != "orchestrator" || gotConfig != "/tmp/project/.ax/config.yaml" {
		t.Fatalf("unexpected dispatch args: target=%q sender=%q config=%q", gotTarget, gotSender, gotConfig)
	}
	result := decodeInterveneResponse(t, resp)
	if result.Status != "woken" {
		t.Fatalf("status = %q, want woken", result.Status)
	}
}

func TestHandleInterveneRetry_CallsEnsureRunnableAfterEnqueue(t *testing.T) {
	var calls int
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		calls++
		if target != "worker" || sender != "orchestrator" {
			t.Fatalf("unexpected dispatch args: target=%q sender=%q", target, sender)
		}
		if configPath != "/tmp/project/.ax/config.yaml" {
			t.Fatalf("unexpected dispatch config %q", configPath)
		}
		if fresh {
			t.Fatal("retry intervene must not request fresh dispatch")
		}
		return nil
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")

	task, err := td.taskStore.CreateWithWorkflow("failing task", "", "worker", "orchestrator", "", types.TaskStartDefault, types.TaskWorkflowParallel, types.TaskPriorityNormal, 0, "start work", "/tmp/project/.ax/config.yaml")
	if err != nil {
		t.Fatalf("create task: %v", err)
	}

	env, _ := NewEnvelope("intervene-retry", MsgInterveneTask, &InterveneTaskPayload{
		ID:     task.ID,
		Action: "retry",
		Note:   "resume after transient failure",
	})
	resp, err := td.handleInterveneTaskEnvelope(env, "orchestrator")
	if err != nil {
		t.Fatalf("handle intervene_task retry: %v", err)
	}
	if calls != 1 {
		t.Fatalf("dispatch called %d times, want 1", calls)
	}
	result := decodeInterveneResponse(t, resp)
	if result.Status != "queued" {
		t.Fatalf("status = %q, want queued", result.Status)
	}
	if result.MessageID == "" {
		t.Fatal("expected retry response to expose the replay message id")
	}
	if td.queue.PendingCount("worker") != 1 {
		t.Fatalf("queue pending for worker = %d, want 1", td.queue.PendingCount("worker"))
	}
	if _, ok := td.wakeScheduler.State("worker"); !ok {
		t.Fatal("expected retry to leave wake scheduler tracking worker")
	}
}
