package daemon

import (
	"testing"
	"time"

	"github.com/ashon/ax/internal/types"
)

func TestTaskStoreCreateDefaultsStartMode(t *testing.T) {
	store := NewTaskStore(t.TempDir())

	task := store.Create("title", "desc", "worker", "orch", "", "", 0)
	if task.StartMode != types.TaskStartDefault {
		t.Fatalf("expected default start mode, got %q", task.StartMode)
	}
	if task.Priority != types.TaskPriorityNormal {
		t.Fatalf("expected default priority, got %q", task.Priority)
	}
}

func TestTaskStoreCreatePersistsFreshStartMode(t *testing.T) {
	store := NewTaskStore(t.TempDir())

	task := store.Create("title", "desc", "worker", "orch", types.TaskStartFresh, types.TaskPriorityHigh, 0)
	if task.StartMode != types.TaskStartFresh {
		t.Fatalf("expected fresh start mode, got %q", task.StartMode)
	}
	if task.Priority != types.TaskPriorityHigh {
		t.Fatalf("expected high priority, got %q", task.Priority)
	}

	got, ok := store.Get(task.ID)
	if !ok {
		t.Fatalf("expected task %q to exist", task.ID)
	}
	if got.StartMode != types.TaskStartFresh {
		t.Fatalf("expected persisted fresh start mode, got %q", got.StartMode)
	}
	if got.Priority != types.TaskPriorityHigh {
		t.Fatalf("expected persisted high priority, got %q", got.Priority)
	}
}

func TestTaskStoreRejectsUnauthorizedStatusChanges(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	task := store.Create("title", "desc", "worker", "orch", "", "", 0)
	status := types.TaskInProgress

	if _, err := store.Update(task.ID, &status, nil, nil, "observer"); err == nil {
		t.Fatal("expected unauthorized updater to be rejected")
	}
}

func TestTaskStoreRejectsNonMonotonicTransitions(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	task := store.Create("title", "desc", "worker", "orch", "", "", 0)
	inProgress := types.TaskInProgress
	if _, err := store.Update(task.ID, &inProgress, nil, nil, "worker"); err != nil {
		t.Fatalf("expected worker to move task in progress: %v", err)
	}

	pending := types.TaskPending
	if _, err := store.Update(task.ID, &pending, nil, nil, "worker"); err == nil {
		t.Fatal("expected in_progress -> pending transition to be rejected")
	}
}

func TestTaskStoreSuppressesDuplicateNoOpLogsWithoutRefreshingTimestamp(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	task := store.Create("title", "desc", "worker", "orch", "", "", 0)
	logMsg := "working on it"

	updated, err := store.Update(task.ID, nil, nil, &logMsg, "worker")
	if err != nil {
		t.Fatalf("first log update failed: %v", err)
	}
	firstUpdatedAt := updated.UpdatedAt
	if len(updated.Logs) != 1 {
		t.Fatalf("expected first log to be appended, got %+v", updated.Logs)
	}

	time.Sleep(10 * time.Millisecond)

	updated, err = store.Update(task.ID, nil, nil, &logMsg, "worker")
	if err != nil {
		t.Fatalf("second log update failed: %v", err)
	}
	if len(updated.Logs) != 1 {
		t.Fatalf("expected duplicate no-op log to be suppressed, got %+v", updated.Logs)
	}
	if !updated.UpdatedAt.Equal(firstUpdatedAt) {
		t.Fatalf("expected duplicate log to avoid refreshing updated_at, got %s -> %s", firstUpdatedAt, updated.UpdatedAt)
	}
}
