package daemon

import (
	"errors"
	"sort"
	"testing"

	"github.com/ashon/ax/internal/types"
)

func TestHandleSendMessage_DispatchesWhenConfigPathProvided(t *testing.T) {
	var gotSocket, gotConfig, gotTarget, gotSender string
	var gotFresh bool
	var calls int
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		calls++
		gotSocket, gotConfig, gotTarget, gotSender, gotFresh = socketPath, configPath, target, sender, fresh
		return nil
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")

	env, _ := NewEnvelope("send-1", MsgSendMessage, &SendMessagePayload{
		To:         "worker",
		Message:    "ping",
		ConfigPath: "/tmp/project/.ax/config.yaml",
	})
	if _, err := td.handleSendMessageEnvelope(env, "orchestrator"); err != nil {
		t.Fatalf("handle send_message: %v", err)
	}
	if calls != 1 {
		t.Fatalf("dispatch called %d times, want 1", calls)
	}
	if gotSocket != "/tmp/ax.sock" || gotConfig != "/tmp/project/.ax/config.yaml" || gotTarget != "worker" || gotSender != "orchestrator" {
		t.Fatalf("unexpected dispatch args: socket=%q config=%q target=%q sender=%q", gotSocket, gotConfig, gotTarget, gotSender)
	}
	if gotFresh {
		t.Fatal("plain send_message without a fresh-mode task ID must not request fresh dispatch")
	}
	if td.queue.PendingCount("worker") != 1 {
		t.Fatal("expected message to be enqueued for worker")
	}
}

func TestHandleSendMessage_SkipsDispatchWhenConfigPathEmpty(t *testing.T) {
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		t.Fatal("dispatch must not run when ConfigPath is empty")
		return nil
	})
	td.register("orchestrator", "")

	env, _ := NewEnvelope("send-2", MsgSendMessage, &SendMessagePayload{
		To:      "worker",
		Message: "ping without dispatch",
	})
	if _, err := td.handleSendMessageEnvelope(env, "orchestrator"); err != nil {
		t.Fatalf("handle send_message: %v", err)
	}
	if td.queue.PendingCount("worker") != 1 {
		t.Fatal("message must still be enqueued even when dispatch is skipped")
	}
}

func TestHandleSendMessage_DetectsFreshModeTaskInBody(t *testing.T) {
	var gotFresh bool
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		gotFresh = fresh
		return nil
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")

	task, err := td.taskStore.CreateWithWorkflow("fresh worker boot", "", "worker", "orchestrator", "", types.TaskStartFresh, types.TaskWorkflowParallel, types.TaskPriorityNormal, 0, "fresh start", "/tmp/project/.ax/config.yaml")
	if err != nil {
		t.Fatalf("create task: %v", err)
	}

	env, _ := NewEnvelope("send-fresh", MsgSendMessage, &SendMessagePayload{
		To:         "worker",
		Message:    "Task ID: " + task.ID + "\n\nBoot fresh",
		ConfigPath: "/tmp/project/.ax/config.yaml",
	})
	if _, err := td.handleSendMessageEnvelope(env, "orchestrator"); err != nil {
		t.Fatalf("handle send_message: %v", err)
	}
	if !gotFresh {
		t.Fatal("expected fresh=true when message references a fresh-mode task authored by the sender")
	}
}

func TestHandleSendMessage_DispatchErrorPropagates(t *testing.T) {
	dispatchErr := errors.New("target unavailable")
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		return dispatchErr
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")

	env, _ := NewEnvelope("send-err", MsgSendMessage, &SendMessagePayload{
		To:         "worker",
		Message:    "ping",
		ConfigPath: "/tmp/project/.ax/config.yaml",
	})
	_, err := td.handleSendMessageEnvelope(env, "orchestrator")
	if !errors.Is(err, dispatchErr) {
		t.Fatalf("expected wrapped dispatch error, got %v", err)
	}
	if td.queue.PendingCount("worker") != 1 {
		t.Fatal("message must remain enqueued even when dispatch fails")
	}
}

func TestHandleBroadcast_DispatchesToEachRecipient(t *testing.T) {
	var mu []string
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		if sender != "orchestrator" {
			t.Fatalf("unexpected sender %q", sender)
		}
		if fresh {
			t.Fatal("broadcast must never request fresh dispatch")
		}
		mu = append(mu, target)
		return nil
	})
	td.register("orchestrator", "/tmp/project/.ax/config.yaml")
	td.register("worker-a", "/tmp/project/.ax/config.yaml")
	td.register("worker-b", "/tmp/project/.ax/config.yaml")

	env, _ := NewEnvelope("broadcast-1", MsgBroadcast, &BroadcastPayload{
		Message:    "team notice",
		ConfigPath: "/tmp/project/.ax/config.yaml",
	})
	if _, err := td.handleBroadcastEnvelope(env, "orchestrator"); err != nil {
		t.Fatalf("handle broadcast: %v", err)
	}
	sort.Strings(mu)
	if len(mu) != 2 || mu[0] != "worker-a" || mu[1] != "worker-b" {
		t.Fatalf("unexpected dispatched targets: %v", mu)
	}
	if td.queue.PendingCount("worker-a") != 1 || td.queue.PendingCount("worker-b") != 1 {
		t.Fatalf("expected each recipient to have one queued message")
	}
}

func TestHandleBroadcast_SkipsDispatchWhenConfigPathEmpty(t *testing.T) {
	td := newDispatchTestDaemon(t, func(socketPath, configPath, target, sender string, fresh bool) error {
		t.Fatal("dispatch must not run for broadcast without ConfigPath")
		return nil
	})
	td.register("orchestrator", "")
	td.register("worker-a", "")

	env, _ := NewEnvelope("broadcast-2", MsgBroadcast, &BroadcastPayload{
		Message: "no dispatch",
	})
	if _, err := td.handleBroadcastEnvelope(env, "orchestrator"); err != nil {
		t.Fatalf("handle broadcast: %v", err)
	}
	if td.queue.PendingCount("worker-a") != 1 {
		t.Fatal("broadcast message must still be enqueued")
	}
}
