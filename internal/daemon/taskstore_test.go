package daemon

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/ashon/ax/internal/types"
)

func TestTaskStoreCreateDefaultsStartMode(t *testing.T) {
	store := NewTaskStore(t.TempDir())

	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
	if task.StartMode != types.TaskStartDefault {
		t.Fatalf("expected default start mode, got %q", task.StartMode)
	}
	if task.Priority != types.TaskPriorityNormal {
		t.Fatalf("expected default priority, got %q", task.Priority)
	}
}

func TestTaskStoreCreatePersistsFreshStartMode(t *testing.T) {
	store := NewTaskStore(t.TempDir())

	task, err := store.Create("title", "desc", "worker", "orch", "", types.TaskStartFresh, types.TaskPriorityHigh, 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
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
	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
	status := types.TaskInProgress

	if _, err := store.Update(task.ID, &status, nil, nil, "observer"); err == nil {
		t.Fatal("expected unauthorized updater to be rejected")
	}
}

func TestTaskStoreRejectsNonMonotonicTransitions(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
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
	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
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

func TestTaskStoreCancelAllowsCreatorAndSetsTerminalState(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}

	cancelled, err := store.Cancel(task.ID, "user requested stop", "orch", nil)
	if err != nil {
		t.Fatalf("cancel task: %v", err)
	}
	if cancelled.Status != types.TaskCancelled {
		t.Fatalf("status=%q, want %q", cancelled.Status, types.TaskCancelled)
	}
	if cancelled.Version != 2 {
		t.Fatalf("version=%d, want 2", cancelled.Version)
	}
	if cancelled.Result == "" {
		t.Fatal("expected cancellation result to be recorded")
	}
}

func TestTaskStoreRemoveHidesTerminalTaskFromList(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
	cancelled, err := store.Cancel(task.ID, "done", "orch", nil)
	if err != nil {
		t.Fatalf("cancel task: %v", err)
	}
	removed, err := store.Remove(task.ID, "archive", "orch", &cancelled.Version)
	if err != nil {
		t.Fatalf("remove task: %v", err)
	}
	if removed.RemovedAt == nil {
		t.Fatal("expected removed_at to be populated")
	}
	if got := store.List("", "", nil); len(got) != 0 {
		t.Fatalf("expected removed task to be hidden from list, got %+v", got)
	}
}

func TestTaskStoreCancelRejectsVersionMismatch(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
	want := int64(99)

	if _, err := store.Cancel(task.ID, "stop", "orch", &want); err == nil {
		t.Fatal("expected version mismatch to be rejected")
	}
}

func TestTaskStoreRecordsFirstActionClaimWithoutChangingStatus(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
	if _, ok := store.RecordDispatch(task.ID, "worker", time.Now()); !ok {
		t.Fatal("expected dispatch metadata to be recorded")
	}

	logMsg := "Inspecting daemon/taskstore.go and tests"
	updated, err := store.Update(task.ID, nil, nil, &logMsg, "worker")
	if err != nil {
		t.Fatalf("update task: %v", err)
	}
	if updated.Status != types.TaskPending {
		t.Fatalf("status=%q, want pending", updated.Status)
	}
	if updated.ClaimedAt == nil || updated.ClaimedBy != "worker" || updated.ClaimSource != "log" {
		t.Fatalf("unexpected claim metadata: %+v", updated)
	}
	if updated.AttemptCount != 1 || updated.LastAttemptAt == nil {
		t.Fatalf("expected first claim attempt metadata, got %+v", updated)
	}
}

func TestTaskStoreRetryResetsBlockedTaskForNextAttempt(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
	if _, ok := store.RecordDispatch(task.ID, "worker", time.Now()); !ok {
		t.Fatal("expected dispatch metadata to be recorded")
	}

	inProgress := types.TaskInProgress
	if _, err := store.Update(task.ID, &inProgress, nil, nil, "worker"); err != nil {
		t.Fatalf("claim task: %v", err)
	}
	blocked := types.TaskBlocked
	blockedReason := "waiting on API credentials"
	if _, err := store.Update(task.ID, &blocked, &blockedReason, nil, "worker"); err != nil {
		t.Fatalf("block task: %v", err)
	}

	retried, err := store.Retry(task.ID, "credentials restored", "orch", nil)
	if err != nil {
		t.Fatalf("retry task: %v", err)
	}
	if retried.Status != types.TaskPending {
		t.Fatalf("status=%q, want pending", retried.Status)
	}
	if retried.ClaimedAt != nil || retried.ClaimedBy != "" || retried.ClaimSource != "" {
		t.Fatalf("expected retry to clear active claim metadata: %+v", retried)
	}
	if retried.Result != "" {
		t.Fatalf("expected retry to clear prior blocked result, got %q", retried.Result)
	}
	if retried.AttemptCount != 1 {
		t.Fatalf("attempt_count=%d, want 1 before next claim", retried.AttemptCount)
	}

	logMsg := "retrying after credentials restored"
	claimedAgain, err := store.Update(task.ID, nil, nil, &logMsg, "worker")
	if err != nil {
		t.Fatalf("claim after retry: %v", err)
	}
	if claimedAgain.AttemptCount != 2 {
		t.Fatalf("attempt_count=%d, want 2 after next claim", claimedAgain.AttemptCount)
	}
}

func TestTaskStorePersistOmitsDerivedObservabilityFields(t *testing.T) {
	dir := t.TempDir()
	store := NewTaskStore(dir)

	task, err := store.Create("title", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create task: %v", err)
	}

	store.mu.Lock()
	live := store.tasks[task.ID]
	live.Rollup = &types.TaskRollup{TotalChildren: 1, PendingChildren: 1}
	live.Sequence = &types.TaskSequenceInfo{
		Mode:     types.TaskWorkflowSerial,
		State:    types.TaskSequenceWaitingTurn,
		Position: 2,
	}
	live.StaleInfo = &types.TaskStaleInfo{
		IsStale: true,
		Reason:  "stale",
	}
	store.persist()
	store.mu.Unlock()

	data, err := os.ReadFile(filepath.Join(dir, taskStateFileName))
	if err != nil {
		t.Fatalf("read persisted task state: %v", err)
	}
	body := string(data)
	if strings.Contains(body, "\"sequence\"") {
		t.Fatalf("expected durable task state to omit sequence, got %s", body)
	}
	if strings.Contains(body, "\"stale_info\"") {
		t.Fatalf("expected durable task state to omit stale_info, got %s", body)
	}
	if !strings.Contains(body, "\"rollup\"") {
		t.Fatalf("expected durable task state to keep rollup, got %s", body)
	}
}

func TestTaskStoreLoadFallsBackToLegacyTasksSnapshotAndClearsDerivedFields(t *testing.T) {
	dir := t.TempDir()
	now := time.Now().UTC()
	legacy := []types.Task{{
		ID:        "task-1",
		Title:     "legacy",
		Assignee:  "worker",
		CreatedBy: "orch",
		Status:    types.TaskPending,
		Sequence: &types.TaskSequenceInfo{
			Mode:     types.TaskWorkflowSerial,
			State:    types.TaskSequenceWaitingTurn,
			Position: 1,
		},
		StaleInfo: &types.TaskStaleInfo{
			IsStale: true,
			Reason:  "legacy stale",
		},
		CreatedAt: now,
		UpdatedAt: now,
	}}
	data, err := json.Marshal(legacy)
	if err != nil {
		t.Fatalf("marshal legacy tasks: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, taskSnapshotFileName), data, 0o600); err != nil {
		t.Fatalf("write legacy task snapshot: %v", err)
	}

	store := NewTaskStore(dir)
	if err := store.Load(); err != nil {
		t.Fatalf("load task store: %v", err)
	}

	got, ok := store.Get("task-1")
	if !ok {
		t.Fatal("expected legacy task to load")
	}
	if got.Sequence != nil {
		t.Fatalf("expected sequence to be recomputed at read time, got %+v", got.Sequence)
	}
	if got.StaleInfo != nil {
		t.Fatalf("expected stale_info to be recomputed at read time, got %+v", got.StaleInfo)
	}
}

func TestTaskStoreRunnableByAssigneeRequiresPriorDispatchAndNoClaim(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	runnable, err := store.Create("runnable", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create runnable task: %v", err)
	}
	if _, ok := store.RecordDispatch(runnable.ID, "worker", time.Now()); !ok {
		t.Fatal("expected runnable task dispatch metadata")
	}
	claimed, err := store.Create("claimed", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create claimed task: %v", err)
	}
	if _, ok := store.RecordDispatch(claimed.ID, "worker", time.Now()); !ok {
		t.Fatal("expected claimed task dispatch metadata")
	}
	logMsg := "starting claimed task"
	if _, err := store.Update(claimed.ID, nil, nil, &logMsg, "worker"); err != nil {
		t.Fatalf("claim task: %v", err)
	}
	undispatched, err := store.Create("undispatched", "desc", "worker", "orch", "", "", "", 0)
	if err != nil {
		t.Fatalf("create undispatched task: %v", err)
	}

	got := store.RunnableByAssignee("worker", time.Now())
	if len(got) != 1 || got[0].ID != runnable.ID {
		t.Fatalf("unexpected runnable tasks: %+v (undispatched=%s)", got, undispatched.ID)
	}
}

func TestTaskStoreCreateWithParentRefreshesRollup(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	parent, err := store.Create("parent", "desc", "orch", "root", "", "", "", 0)
	if err != nil {
		t.Fatalf("create parent: %v", err)
	}
	child, err := store.Create("child", "desc", "worker", "orch", parent.ID, "", "", 0)
	if err != nil {
		t.Fatalf("create child: %v", err)
	}

	refreshedParent, ok := store.Get(parent.ID)
	if !ok {
		t.Fatalf("expected parent %q to exist", parent.ID)
	}
	if len(refreshedParent.ChildTaskIDs) != 1 || refreshedParent.ChildTaskIDs[0] != child.ID {
		t.Fatalf("unexpected child task ids: %+v", refreshedParent.ChildTaskIDs)
	}
	if refreshedParent.Rollup == nil || refreshedParent.Rollup.TotalChildren != 1 || refreshedParent.Rollup.PendingChildren != 1 {
		t.Fatalf("unexpected parent rollup: %+v", refreshedParent.Rollup)
	}
}

func TestTaskStoreChildTerminalUpdateRequestsParentReconciliation(t *testing.T) {
	store := NewTaskStore(t.TempDir())
	parent, err := store.Create("parent", "desc", "orch", "root", "", "", "", 0)
	if err != nil {
		t.Fatalf("create parent: %v", err)
	}
	child, err := store.Create("child", "desc", "worker", "orch", parent.ID, "", "", 0)
	if err != nil {
		t.Fatalf("create child: %v", err)
	}
	done := types.TaskCompleted
	result := "Changed files: internal/daemon/taskstore.go\nValidation: go test ./internal/daemon/..."
	if _, err := store.Update(child.ID, &done, &result, nil, "worker"); err != nil {
		t.Fatalf("complete child: %v", err)
	}

	refreshedParent, ok := store.Get(parent.ID)
	if !ok {
		t.Fatalf("expected parent %q to exist", parent.ID)
	}
	if refreshedParent.Rollup == nil || !refreshedParent.Rollup.AllChildrenTerminal || !refreshedParent.Rollup.NeedsParentReconciliation {
		t.Fatalf("unexpected parent rollup after child completion: %+v", refreshedParent.Rollup)
	}
	if len(refreshedParent.Logs) == 0 || refreshedParent.Logs[len(refreshedParent.Logs)-1].Message == "" {
		t.Fatalf("expected parent rollup log to be appended: %+v", refreshedParent.Logs)
	}
}
