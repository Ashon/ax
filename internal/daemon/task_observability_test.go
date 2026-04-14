package daemon

import (
	"testing"
	"time"

	"github.com/ashon/ax/internal/types"
)

func TestEnrichTaskComputesStaleInfoAndDefaultsPriority(t *testing.T) {
	stateDir := t.TempDir()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}

	task := types.Task{
		ID:                "task-1",
		Title:             "Investigate stale worker",
		Assignee:          "worker",
		CreatedBy:         "orch",
		Status:            types.TaskInProgress,
		StaleAfterSeconds: 30,
		UpdatedAt:         time.Now().Add(-45 * time.Second),
	}
	d.queue.Enqueue("orch", "worker", "follow up on task-1")
	d.history.Append("orch", "worker", "Task ID: task-1")

	enriched := d.enrichTask(task)
	if enriched.Priority != types.TaskPriorityNormal {
		t.Fatalf("expected default normal priority, got %q", enriched.Priority)
	}
	if enriched.StaleInfo == nil {
		t.Fatal("expected stale_info to be populated")
	}
	if !enriched.StaleInfo.IsStale {
		t.Fatal("expected task to be stale")
	}
	if enriched.StaleInfo.PendingMessages != 1 {
		t.Fatalf("expected one pending message, got %d", enriched.StaleInfo.PendingMessages)
	}
	if enriched.StaleInfo.LastMessageAt == nil {
		t.Fatal("expected last_message_at to be set")
	}
}

func TestEnrichTaskMarksPendingTaskWithNoQueuedMessageAsDiverged(t *testing.T) {
	stateDir := t.TempDir()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}

	task := types.Task{
		ID:        "task-2",
		Title:     "Re-dispatch work",
		Assignee:  "worker",
		CreatedBy: "orch",
		Status:    types.TaskPending,
		UpdatedAt: time.Now(),
		CreatedAt: time.Now(),
		StartMode: types.TaskStartDefault,
		Priority:  types.TaskPriorityLow,
		Logs:      nil,
		StaleInfo: nil,
	}

	enriched := d.enrichTask(task)
	if enriched.StaleInfo == nil {
		t.Fatal("expected stale_info to be populated")
	}
	if !enriched.StaleInfo.StateDivergence {
		t.Fatal("expected state divergence when pending task has no queued message")
	}
	if enriched.StaleInfo.RecommendedAction == "" {
		t.Fatal("expected recommended action for divergence")
	}
}

func TestEnrichTaskIncludesPendingWakeState(t *testing.T) {
	stateDir := t.TempDir()
	d := &Daemon{
		queue:         NewMessageQueue(),
		history:       NewHistory(stateDir, 50),
		registry:      NewRegistry(),
		taskStore:     NewTaskStore(stateDir),
		wakeScheduler: NewWakeScheduler(NewMessageQueue(), nil),
	}
	d.wakeScheduler.Schedule("worker", "orch")

	task := types.Task{
		ID:        "task-3",
		Title:     "Wake blocked worker",
		Assignee:  "worker",
		CreatedBy: "orch",
		Status:    types.TaskPending,
		UpdatedAt: time.Now(),
	}

	enriched := d.enrichTask(task)
	if enriched.StaleInfo == nil {
		t.Fatal("expected stale_info to be populated")
	}
	if !enriched.StaleInfo.WakePending {
		t.Fatal("expected wake_pending to be true")
	}
	if enriched.StaleInfo.NextWakeRetryAt == nil {
		t.Fatal("expected next_wake_retry_at to be populated")
	}
}
