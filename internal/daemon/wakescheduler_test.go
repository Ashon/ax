package daemon

import (
	"io"
	"log"
	"testing"
	"time"
)

func restoreWakeSchedulerStubs(t *testing.T) {
	t.Helper()

	oldSessionExists := wakeSchedulerSessionExists
	oldSessionIdle := wakeSchedulerSessionIdle
	oldWakeWorkspace := wakeSchedulerWakeWorkspace

	t.Cleanup(func() {
		wakeSchedulerSessionExists = oldSessionExists
		wakeSchedulerSessionIdle = oldSessionIdle
		wakeSchedulerWakeWorkspace = oldWakeWorkspace
	})
}

func TestWakeSchedulerStopsRetryingAfterSuccessfulWakeWhenPolicyClears(t *testing.T) {
	restoreWakeSchedulerStubs(t)

	queue := NewMessageQueue()
	queue.Enqueue("orch", "worker", "follow up")

	scheduler := NewWakeScheduler(queue, log.New(io.Discard, "", 0))
	scheduler.SetRetryAfterSuccessfulWake(func(workspace string) bool {
		if workspace != "worker" {
			t.Fatalf("unexpected workspace %q", workspace)
		}
		return false
	})

	wakeCalls := 0
	wakeSchedulerSessionExists = func(name string) bool { return name == "worker" }
	wakeSchedulerSessionIdle = func(name string) bool { return name == "worker" }
	wakeSchedulerWakeWorkspace = func(name, prompt string) error {
		wakeCalls++
		if name != "worker" {
			t.Fatalf("unexpected wake target %q", name)
		}
		return nil
	}

	scheduler.Schedule("worker", "orch")
	scheduler.mu.Lock()
	scheduler.pending["worker"].NextRetry = time.Now().Add(-time.Second)
	scheduler.mu.Unlock()

	scheduler.process()

	if wakeCalls != 1 {
		t.Fatalf("wake calls = %d, want 1", wakeCalls)
	}
	if _, ok := scheduler.State("worker"); ok {
		t.Fatal("expected successful wake policy to clear pending retries")
	}
}

func TestWakeSchedulerKeepsRetryingAfterSuccessfulWakeWhenPolicyAllows(t *testing.T) {
	restoreWakeSchedulerStubs(t)

	queue := NewMessageQueue()
	queue.Enqueue("orch", "worker", "follow up")

	scheduler := NewWakeScheduler(queue, log.New(io.Discard, "", 0))
	scheduler.SetRetryAfterSuccessfulWake(func(workspace string) bool { return true })

	wakeSchedulerSessionExists = func(name string) bool { return name == "worker" }
	wakeSchedulerSessionIdle = func(name string) bool { return name == "worker" }
	wakeSchedulerWakeWorkspace = func(name, prompt string) error { return nil }

	scheduler.Schedule("worker", "orch")
	scheduler.mu.Lock()
	scheduler.pending["worker"].NextRetry = time.Now().Add(-time.Second)
	scheduler.mu.Unlock()

	scheduler.process()

	state, ok := scheduler.State("worker")
	if !ok {
		t.Fatal("expected successful wake to remain scheduled when policy allows")
	}
	if state.Attempts != 1 {
		t.Fatalf("attempts = %d, want 1", state.Attempts)
	}
	if !state.NextRetry.After(time.Now()) {
		t.Fatalf("expected next retry in the future, got %v", state.NextRetry)
	}
}
