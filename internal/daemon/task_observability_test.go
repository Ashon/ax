package daemon

import (
	"net"
	"strings"
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
	if enriched.StaleInfo.StateDivergence {
		t.Fatal("did not expect divergence before a task-aware dispatch is recorded")
	}
	if enriched.StaleInfo.ClaimState != "undispatched" {
		t.Fatalf("claim_state=%q, want undispatched", enriched.StaleInfo.ClaimState)
	}
}

func TestEnrichTaskMarksDispatchedUnclaimedTaskAsRecoverable(t *testing.T) {
	stateDir := t.TempDir()
	now := time.Now()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}

	task := types.Task{
		ID:             "task-2b",
		Title:          "Re-dispatch work",
		Assignee:       "worker",
		CreatedBy:      "orch",
		Status:         types.TaskPending,
		LastDispatchAt: &now,
		UpdatedAt:      now,
		CreatedAt:      now,
	}

	enriched := d.enrichTask(task)
	if enriched.StaleInfo == nil {
		t.Fatal("expected stale_info to be populated")
	}
	if !enriched.StaleInfo.StateDivergence {
		t.Fatal("expected divergence once dispatch is gone and no first task-flow action exists")
	}
	if !enriched.StaleInfo.Runnable || !enriched.StaleInfo.RecoveryEligible {
		t.Fatalf("expected runnable recovery hint, got %+v", enriched.StaleInfo)
	}
	if enriched.StaleInfo.RecommendedAction == "" {
		t.Fatal("expected recommended action for divergence")
	}
}

func TestEnrichTaskTreatsBlockedTaskAsRecoverable(t *testing.T) {
	stateDir := t.TempDir()
	now := time.Now()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}

	task := types.Task{
		ID:           "task-blocked",
		Title:        "Wait for credentials",
		Assignee:     "worker",
		CreatedBy:    "orch",
		Status:       types.TaskBlocked,
		AttemptCount: 1,
		UpdatedAt:    now,
		CreatedAt:    now,
	}

	enriched := d.enrichTask(task)
	if enriched.StaleInfo == nil {
		t.Fatal("expected stale_info to be populated")
	}
	if !enriched.StaleInfo.RecoveryEligible {
		t.Fatalf("expected blocked task to be recoverable, got %+v", enriched.StaleInfo)
	}
	if enriched.StaleInfo.Reason == "" || !strings.Contains(enriched.StaleInfo.Reason, "blocked") {
		t.Fatalf("expected blocked reason, got %+v", enriched.StaleInfo)
	}
}

func TestEnrichTaskNotesFreshStartBarrierUntilWorkerReconnects(t *testing.T) {
	stateDir := t.TempDir()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}

	workerConnA, workerConnB := net.Pipe()
	defer workerConnA.Close()
	defer workerConnB.Close()
	d.registry.Register("worker", "", "", workerConnA)

	time.Sleep(10 * time.Millisecond)
	now := time.Now()
	task := types.Task{
		ID:             "task-fresh",
		Title:          "Fresh start work",
		Assignee:       "worker",
		CreatedBy:      "orch",
		Status:         types.TaskPending,
		StartMode:      types.TaskStartFresh,
		LastDispatchAt: &now,
		UpdatedAt:      now,
		CreatedAt:      now,
	}

	enriched := d.enrichTask(task)
	if enriched.StaleInfo == nil {
		t.Fatal("expected stale_info to be populated")
	}
	if enriched.StaleInfo.ClaimState != "awaiting_claim" {
		t.Fatalf("claim_state=%q, want awaiting_claim", enriched.StaleInfo.ClaimState)
	}
	if enriched.StaleInfo.ClaimStateNote == "" || !strings.Contains(enriched.StaleInfo.ClaimStateNote, "fresh-context") {
		t.Fatalf("expected fresh-context note, got %+v", enriched.StaleInfo)
	}
	if enriched.StaleInfo.RecommendedAction == "" {
		t.Fatalf("expected fresh barrier recommendation, got %+v", enriched.StaleInfo)
	}
}

func TestEnrichTaskTreatsClaimedPendingTaskAsClaimedNotDiverged(t *testing.T) {
	stateDir := t.TempDir()
	now := time.Now()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}

	task := types.Task{
		ID:             "task-2c",
		Title:          "Inspect files",
		Assignee:       "worker",
		CreatedBy:      "orch",
		Status:         types.TaskPending,
		LastDispatchAt: &now,
		ClaimedAt:      &now,
		ClaimedBy:      "worker",
		ClaimSource:    "log",
		UpdatedAt:      now,
	}

	enriched := d.enrichTask(task)
	if enriched.StaleInfo == nil {
		t.Fatal("expected stale_info to be populated")
	}
	if enriched.StaleInfo.StateDivergence {
		t.Fatalf("did not expect divergence after first task-flow claim: %+v", enriched.StaleInfo)
	}
	if enriched.StaleInfo.ClaimState != "claimed" {
		t.Fatalf("claim_state=%q, want claimed", enriched.StaleInfo.ClaimState)
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

func TestEnrichTaskSurfacesParentReconciliationNeed(t *testing.T) {
	stateDir := t.TempDir()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}

	task := types.Task{
		ID:        "parent",
		Title:     "Umbrella task",
		Assignee:  "orch",
		Status:    types.TaskInProgress,
		UpdatedAt: time.Now(),
		Rollup: &types.TaskRollup{
			TotalChildren:             2,
			CompletedChildren:         2,
			TerminalChildren:          2,
			AllChildrenTerminal:       true,
			NeedsParentReconciliation: true,
			Summary:                   "Child rollup: total=2 active=0 completed=2 failed=0 cancelled=0 pending=0 in_progress=0 blocked=0. All child tasks are terminal; parent reconciliation is still required.",
		},
	}

	enriched := d.enrichTask(task)
	if enriched.StaleInfo == nil {
		t.Fatal("expected stale_info to be populated")
	}
	if !enriched.StaleInfo.StateDivergence {
		t.Fatal("expected parent reconciliation to surface as divergence")
	}
	if enriched.StaleInfo.RecommendedAction == "" {
		t.Fatal("expected reconciliation action guidance")
	}
}

func TestEnrichTaskMarksSerialChildAsWaitingTurn(t *testing.T) {
	stateDir := t.TempDir()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}

	parent, err := d.taskStore.CreateWithWorkflow("parent", "desc", "orch", "root", "", "", types.TaskWorkflowSerial, "", 0, "")
	if err != nil {
		t.Fatalf("create parent: %v", err)
	}
	first, err := d.taskStore.Create("first", "desc", "worker", "orch", parent.ID, "", "", 0)
	if err != nil {
		t.Fatalf("create first child: %v", err)
	}
	second, err := d.taskStore.Create("second", "desc", "worker", "orch", parent.ID, "", "", 0)
	if err != nil {
		t.Fatalf("create second child: %v", err)
	}

	enriched := d.enrichTask(*second)
	if enriched.Sequence == nil {
		t.Fatal("expected sequence info for serial child")
	}
	if enriched.Sequence.State != types.TaskSequenceWaitingTurn {
		t.Fatalf("sequence state = %q, want %q", enriched.Sequence.State, types.TaskSequenceWaitingTurn)
	}
	if enriched.Sequence.WaitingOnTaskID != first.ID {
		t.Fatalf("waiting_on_task_id = %q, want %q", enriched.Sequence.WaitingOnTaskID, first.ID)
	}
	if enriched.StaleInfo == nil {
		t.Fatal("expected stale_info to be populated")
	}
	if enriched.StaleInfo.ClaimState != string(types.TaskSequenceWaitingTurn) {
		t.Fatalf("claim_state = %q, want %q", enriched.StaleInfo.ClaimState, types.TaskSequenceWaitingTurn)
	}
	if enriched.StaleInfo.Runnable {
		t.Fatalf("waiting-turn task should not be runnable: %+v", enriched.StaleInfo)
	}
}
