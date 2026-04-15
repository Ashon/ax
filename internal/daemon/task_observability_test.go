package daemon

import (
	"encoding/json"
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
	d.registry.Register("worker", "", "", "", 0, workerConnA)

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

func TestHandleReadMessagesSchedulesClaimFollowUpAfterConsumingTaskDispatch(t *testing.T) {
	stateDir := t.TempDir()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}
	d.wakeScheduler = NewWakeScheduler(d.queue, nil)
	d.wakeScheduler.SetQueueRefiller(d.recoverRunnableTaskMessages)

	task, err := d.taskStore.CreateWithWorkflow("claim follow-up", "desc", "worker", "orch", "", "", types.TaskWorkflowParallel, "", 0, "Inspect and claim")
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
	msg := taskAwareMessage("orch", "worker", task.DispatchMessage)
	msg = d.queue.EnqueueMessage(msg)
	if _, ok := d.taskStore.RecordDispatch(msg.TaskID, msg.To, msg.CreatedAt); !ok {
		t.Fatal("expected task dispatch metadata to record")
	}

	env, _ := NewEnvelope("read-task", MsgReadMessages, &ReadMessagesPayload{Limit: 10})
	resp, err := d.handleReadMessagesEnvelope(env, "worker")
	if err != nil {
		t.Fatalf("handle read_messages: %v", err)
	}
	var payload ResponsePayload
	if err := resp.DecodePayload(&payload); err != nil {
		t.Fatalf("decode read payload: %v", err)
	}
	var readResp ReadMessagesResponse
	if err := json.Unmarshal(payload.Data, &readResp); err != nil {
		t.Fatalf("unmarshal read response: %v", err)
	}
	if len(readResp.Messages) != 1 || readResp.Messages[0].TaskID != task.ID {
		t.Fatalf("expected consumed task dispatch, got %+v", readResp.Messages)
	}
	if d.queue.PendingCount("worker") != 0 {
		t.Fatalf("expected queue to be empty after read, got %d pending message(s)", d.queue.PendingCount("worker"))
	}
	wakeState, ok := d.wakeScheduler.State("worker")
	if !ok {
		t.Fatal("expected claim follow-up wake to remain scheduled after dispatch consumption")
	}
	if wakeState.Sender != "orch" {
		t.Fatalf("wake sender = %q, want orch", wakeState.Sender)
	}
}

func TestRecoverRunnableTaskMessagesRequeuesConsumedUnclaimedTaskDispatch(t *testing.T) {
	stateDir := t.TempDir()
	d := &Daemon{
		queue:     NewMessageQueue(),
		history:   NewHistory(stateDir, 50),
		registry:  NewRegistry(),
		taskStore: NewTaskStore(stateDir),
	}
	d.wakeScheduler = NewWakeScheduler(d.queue, nil)

	task, err := d.taskStore.CreateWithWorkflow("recover", "desc", "worker", "orch", "", "", types.TaskWorkflowParallel, "", 0, "Inspect and claim")
	if err != nil {
		t.Fatalf("create task: %v", err)
	}
	if _, ok := d.taskStore.RecordDispatch(task.ID, "worker", time.Now()); !ok {
		t.Fatal("expected task dispatch metadata to record")
	}
	if got := d.recoverRunnableTaskMessages("worker"); got != 1 {
		t.Fatalf("recoverRunnableTaskMessages() = %d, want 1", got)
	}
	pending := d.queue.Pending("worker")
	if len(pending) != 1 || messageTaskID(pending[0]) != task.ID || pending[0].Content != task.DispatchMessage {
		t.Fatalf("expected one rehydrated canonical dispatch, got %+v", pending)
	}

	logMsg := "claiming recovered task"
	if _, err := d.taskStore.Update(task.ID, nil, nil, &logMsg, "worker"); err != nil {
		t.Fatalf("claim recovered task: %v", err)
	}
	_ = d.queue.RemoveTaskMessages("worker", task.ID)
	if got := d.recoverRunnableTaskMessages("worker"); got != 0 {
		t.Fatalf("expected claimed task to stop rehydrating, got %d", got)
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
