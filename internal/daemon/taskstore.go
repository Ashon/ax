package daemon

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/ashon/ax/internal/types"
	"github.com/google/uuid"
)

// TaskStore owns persistent task state for the daemon and returns defensive
// copies so callers cannot mutate the in-memory store without going through its
// validation rules.
type TaskStore struct {
	mu       sync.RWMutex
	tasks    map[string]*types.Task
	filePath string
	legacy   string
}

const (
	operatorWorkspaceName = "_cli"
	taskStateFileName     = "tasks-state.json"
	taskSnapshotFileName  = "tasks.json"
)

// NewTaskStore creates a persistent task store rooted in the daemon state dir.
// Durable task state is serialized to tasks-state.json under that directory.
// The legacy tasks.json path is still accepted on load so existing daemon state
// continues to work after upgrades.
func NewTaskStore(stateDir string) *TaskStore {
	return &TaskStore{
		tasks:    make(map[string]*types.Task),
		filePath: filepath.Join(stateDir, taskStateFileName),
		legacy:   filepath.Join(stateDir, taskSnapshotFileName),
	}
}

// Load rehydrates tasks from disk. A missing or empty tasks file is treated as
// an empty store.
func (s *TaskStore) Load() error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.filePath == "" {
		return nil
	}
	data, err := os.ReadFile(s.filePath)
	if err != nil && os.IsNotExist(err) && s.legacy != "" && s.legacy != s.filePath {
		data, err = os.ReadFile(s.legacy)
	}
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	if len(data) == 0 {
		s.tasks = make(map[string]*types.Task)
		return nil
	}
	var tasks []types.Task
	if err := json.Unmarshal(data, &tasks); err != nil {
		return err
	}
	loaded := make(map[string]*types.Task, len(tasks))
	for _, task := range tasks {
		cp := copyTask(&task)
		clearDerivedTaskFields(cp)
		loaded[task.ID] = cp
	}
	s.tasks = loaded
	return nil
}

// TasksFilePath returns the path to the materialized task snapshot file used by
// external readers such as watch.
func TasksFilePath(socketPath string) string {
	return filepath.Join(filepath.Dir(ExpandSocketPath(socketPath)), taskSnapshotFileName)
}

// Create inserts a new pending task, applying default start mode and priority
// when omitted, and persists the updated store before returning the live task.
func (s *TaskStore) Create(title, description, assignee, createdBy, parentTaskID string, startMode types.TaskStartMode, priority types.TaskPriority, staleAfterSeconds int) (*types.Task, error) {
	return s.CreateWithWorkflow(title, description, assignee, createdBy, parentTaskID, startMode, types.TaskWorkflowParallel, priority, staleAfterSeconds, "", "")
}

// CreateWithWorkflow is the daemon's internal constructor used when richer
// dispatch and sequencing metadata must be persisted with the task record.
func (s *TaskStore) CreateWithWorkflow(title, description, assignee, createdBy, parentTaskID string, startMode types.TaskStartMode, workflowMode types.TaskWorkflowMode, priority types.TaskPriority, staleAfterSeconds int, dispatchBody, dispatchConfigPath string) (*types.Task, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if startMode == "" {
		startMode = types.TaskStartDefault
	}
	if workflowMode == "" {
		workflowMode = types.TaskWorkflowParallel
	}
	if priority == "" {
		priority = types.TaskPriorityNormal
	}

	now := time.Now()
	task := &types.Task{
		ID:                 uuid.New().String(),
		Title:              title,
		Description:        description,
		Assignee:           assignee,
		CreatedBy:          createdBy,
		ParentTaskID:       strings.TrimSpace(parentTaskID),
		Version:            1,
		Status:             types.TaskPending,
		StartMode:          startMode,
		WorkflowMode:       workflowMode,
		Priority:           priority,
		StaleAfterSeconds:  staleAfterSeconds,
		DispatchConfigPath: strings.TrimSpace(dispatchConfigPath),
		CreatedAt:          now,
		UpdatedAt:          now,
	}
	if trimmed := strings.TrimSpace(dispatchBody); trimmed != "" {
		task.DispatchMessage = formatTaskDispatchMessage(task.ID, trimmed)
	}
	s.tasks[task.ID] = task
	if task.ParentTaskID != "" {
		parent, ok := s.tasks[task.ParentTaskID]
		if !ok {
			delete(s.tasks, task.ID)
			return nil, fmt.Errorf("parent task %q not found", task.ParentTaskID)
		}
		if parent.RemovedAt != nil {
			delete(s.tasks, task.ID)
			return nil, fmt.Errorf("parent task %q has been removed", task.ParentTaskID)
		}
		if !slices.Contains(parent.ChildTaskIDs, task.ID) {
			parent.ChildTaskIDs = append(parent.ChildTaskIDs, task.ID)
			parent.Version++
			parent.UpdatedAt = now
		}
		s.refreshParentRollupLocked(parent.ID, now)
	}
	s.persist()
	return task, nil
}

// Get returns a defensive copy of the task with the given ID.
func (s *TaskStore) Get(id string) (*types.Task, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	task, ok := s.tasks[id]
	if !ok {
		return nil, false
	}
	return copyTask(task), true
}

// Update validates the caller, applies status/result/log changes, suppresses
// duplicate no-op status logs, and persists only when the task actually changed.
func (s *TaskStore) Update(id string, status *types.TaskStatus, result *string, logMsg *string, workspace string) (*types.Task, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	task, ok := s.tasks[id]
	if !ok {
		return nil, fmt.Errorf("task %q not found", id)
	}
	if err := validateTaskUpdate(task, status, result, logMsg, workspace); err != nil {
		return nil, err
	}

	now := time.Now()
	changed := false
	if workspace == task.Assignee && hasTaskAction(status, result, logMsg) {
		if markTaskClaimed(task, workspace, claimSource(status, result, logMsg), now) {
			changed = true
		}
	}
	if status != nil {
		if task.Status != *status {
			task.Status = *status
			changed = true
		}
		if *status == types.TaskBlocked || isTerminalTaskStatus(*status) {
			if clearTaskRetryState(task) {
				changed = true
			}
		}
	}
	if result != nil {
		if task.Result != *result {
			task.Result = *result
			changed = true
		}
	}
	if logMsg != nil {
		if !isDuplicateTaskLog(task, workspace, *logMsg, now) {
			task.Logs = append(task.Logs, types.TaskLog{
				Timestamp: now,
				Workspace: workspace,
				Message:   *logMsg,
			})
			changed = true
		}
	}
	if changed {
		task.Version++
		task.UpdatedAt = now
		s.refreshParentRollupLocked(task.ParentTaskID, now)
		s.persist()
	}

	return copyTask(task), nil
}

// Retry resets a blocked or in-flight task back to pending so the daemon can
// re-dispatch canonical wake/input for a fresh execution attempt.
func (s *TaskStore) Retry(id, note, workspace string, expectedVersion *int64) (*types.Task, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	task, ok := s.tasks[id]
	if !ok {
		return nil, fmt.Errorf("task %q not found", id)
	}
	if err := validateTaskControl(task, workspace, expectedVersion, true); err != nil {
		return nil, err
	}
	if task.Status != types.TaskPending && task.Status != types.TaskInProgress && task.Status != types.TaskBlocked {
		return nil, fmt.Errorf("task %q is not pending/in_progress/blocked", id)
	}

	now := time.Now()
	task.Status = types.TaskPending
	task.Result = ""
	clearTaskClaim(task)
	clearTaskRetryState(task)
	msg := fmt.Sprintf("Recovery action: retry requested by %s", workspace)
	if trimmed := strings.TrimSpace(note); trimmed != "" {
		msg += ": " + trimmed
	}
	task.Logs = append(task.Logs, types.TaskLog{
		Timestamp: now,
		Workspace: workspace,
		Message:   msg,
	})
	task.Version++
	task.UpdatedAt = now
	s.refreshParentRollupLocked(task.ParentTaskID, now)
	s.persist()
	return copyTask(task), nil
}

// Cancel marks a non-terminal task as cancelled. Creators, assignees, and the
// CLI operator identity may cancel tasks.
func (s *TaskStore) Cancel(id, reason, workspace string, expectedVersion *int64) (*types.Task, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	task, ok := s.tasks[id]
	if !ok {
		return nil, fmt.Errorf("task %q not found", id)
	}
	if err := validateTaskControl(task, workspace, expectedVersion, true); err != nil {
		return nil, err
	}
	if isTerminalTaskStatus(task.Status) {
		return nil, fmt.Errorf("task %q is already terminal (%s)", id, task.Status)
	}

	now := time.Now()
	msg := fmt.Sprintf("Cancelled by %s", workspace)
	if trimmed := strings.TrimSpace(reason); trimmed != "" {
		msg += ": " + trimmed
	}
	if workspace == task.Assignee {
		_ = markTaskClaimed(task, workspace, "cancel", now)
	}
	task.Status = types.TaskCancelled
	task.Result = msg
	task.Logs = append(task.Logs, types.TaskLog{
		Timestamp: now,
		Workspace: workspace,
		Message:   msg,
	})
	task.Version++
	task.UpdatedAt = now
	s.refreshParentRollupLocked(task.ParentTaskID, now)
	s.persist()
	return copyTask(task), nil
}

// Remove archives a terminal task so it no longer appears in list results.
// The task remains retrievable by ID for audit/debug purposes.
func (s *TaskStore) Remove(id, reason, workspace string, expectedVersion *int64) (*types.Task, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	task, ok := s.tasks[id]
	if !ok {
		return nil, fmt.Errorf("task %q not found", id)
	}
	if err := validateTaskControl(task, workspace, expectedVersion, false); err != nil {
		return nil, err
	}
	if task.RemovedAt != nil {
		return copyTask(task), nil
	}
	if !isTerminalTaskStatus(task.Status) {
		return nil, fmt.Errorf("task %q must be completed, failed, or cancelled before remove", id)
	}

	now := time.Now()
	task.RemovedAt = &now
	task.RemovedBy = workspace
	task.RemoveReason = strings.TrimSpace(reason)
	task.Version++
	s.refreshParentRollupLocked(task.ParentTaskID, now)
	s.persist()
	return copyTask(task), nil
}

// List returns defensive copies of tasks filtered by assignee, creator, and
// status when those filters are provided.
func (s *TaskStore) List(assignee, createdBy string, status *types.TaskStatus) []types.Task {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make([]types.Task, 0, len(s.tasks))
	for _, task := range s.tasks {
		if task.RemovedAt != nil {
			continue
		}
		if assignee != "" && task.Assignee != assignee {
			continue
		}
		if createdBy != "" && task.CreatedBy != createdBy {
			continue
		}
		if status != nil && task.Status != *status {
			continue
		}
		result = append(result, *copyTask(task))
	}
	return result
}

// RecordDispatch tracks that a task-aware message was explicitly dispatched to
// the assignee. Delivery/read is separate from claim; claim occurs only when
// the assignee emits the first task-flow action.
func (s *TaskStore) RecordDispatch(id, to string, when time.Time) (*types.Task, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()

	task, ok := s.tasks[id]
	if !ok || task.RemovedAt != nil || task.Assignee != to {
		return nil, false
	}
	task.DispatchCount++
	if when.IsZero() {
		when = time.Now()
	}
	task.LastDispatchAt = &when
	task.UpdatedAt = when
	task.Version++
	s.persist()
	return copyTask(task), true
}

// RunnableByAssignee returns task snapshots whose next execution step should be
// recoverable from the task registry without relying on a pre-existing queue
// entry.
func (s *TaskStore) RunnableByAssignee(assignee string, now time.Time) []types.Task {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make([]types.Task, 0)
	for _, task := range s.tasks {
		if task.RemovedAt != nil || task.Assignee != assignee {
			continue
		}
		if task.Status != types.TaskPending || task.LastDispatchAt == nil || task.ClaimedAt != nil {
			continue
		}
		if task.NextRetryAt != nil && task.NextRetryAt.After(now) {
			continue
		}
		result = append(result, *copyTask(task))
	}
	return result
}

func (s *TaskStore) persist() {
	if s.filePath == "" {
		return
	}
	tasks := make([]types.Task, 0, len(s.tasks))
	for _, t := range s.tasks {
		persisted := copyTask(t)
		clearDerivedTaskFields(persisted)
		tasks = append(tasks, *persisted)
	}
	sort.Slice(tasks, func(i, j int) bool {
		return tasks[i].ID < tasks[j].ID
	})
	data, err := json.Marshal(tasks)
	if err != nil {
		return
	}
	_ = writeFileAtomic(s.filePath, data, 0o600)
}

// Snapshot returns copies of every stored task without applying additional
// enrichment.
func (s *TaskStore) Snapshot() []types.Task {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make([]types.Task, 0, len(s.tasks))
	for _, task := range s.tasks {
		result = append(result, *copyTask(task))
	}
	return result
}

func hasTaskAction(status *types.TaskStatus, result *string, logMsg *string) bool {
	return status != nil || result != nil || logMsg != nil
}

func claimSource(status *types.TaskStatus, result *string, logMsg *string) string {
	switch {
	case status != nil:
		return "status:" + string(*status)
	case result != nil:
		return "result"
	case logMsg != nil:
		return "log"
	default:
		return ""
	}
}

func markTaskClaimed(task *types.Task, workspace, source string, now time.Time) bool {
	if task.ClaimedAt != nil || workspace != task.Assignee {
		return false
	}
	task.ClaimedAt = &now
	task.ClaimedBy = workspace
	task.ClaimSource = source
	task.AttemptCount++
	task.LastAttemptAt = &now
	task.NextRetryAt = nil
	return true
}

func clearTaskClaim(task *types.Task) {
	task.ClaimedAt = nil
	task.ClaimedBy = ""
	task.ClaimSource = ""
}

func clearTaskRetryState(task *types.Task) bool {
	if task.NextRetryAt == nil {
		return false
	}
	task.NextRetryAt = nil
	return true
}

func (s *TaskStore) refreshParentRollupLocked(parentID string, now time.Time) {
	parentID = strings.TrimSpace(parentID)
	if parentID == "" {
		return
	}
	parent, ok := s.tasks[parentID]
	if !ok || parent.RemovedAt != nil {
		return
	}

	rollup := summarizeTaskRollup(parent, s.tasks)
	changed := !taskRollupEqual(parent.Rollup, rollup)
	parent.Rollup = rollup
	if rollup == nil {
		return
	}

	logMsg := rollup.Summary
	if logMsg != "" && shouldAppendRollupLog(parent, logMsg) {
		parent.Logs = append(parent.Logs, types.TaskLog{
			Timestamp: now,
			Workspace: parent.Assignee,
			Message:   logMsg,
		})
		changed = true
	}
	if changed {
		parent.Version++
		parent.UpdatedAt = now
	}
}

func summarizeTaskRollup(parent *types.Task, all map[string]*types.Task) *types.TaskRollup {
	if len(parent.ChildTaskIDs) == 0 {
		return nil
	}

	rollup := &types.TaskRollup{TotalChildren: len(parent.ChildTaskIDs)}
	for _, childID := range parent.ChildTaskIDs {
		child, ok := all[childID]
		if !ok {
			continue
		}
		switch child.Status {
		case types.TaskPending:
			rollup.PendingChildren++
		case types.TaskInProgress:
			rollup.InProgressChildren++
		case types.TaskBlocked:
			rollup.BlockedChildren++
		case types.TaskCompleted:
			rollup.CompletedChildren++
			rollup.TerminalChildren++
		case types.TaskFailed:
			rollup.FailedChildren++
			rollup.TerminalChildren++
		case types.TaskCancelled:
			rollup.CancelledChildren++
			rollup.TerminalChildren++
		}
		if child.Status == types.TaskPending || child.Status == types.TaskInProgress {
			rollup.ActiveChildren++
		}
		if rollup.LastChildUpdateAt == nil || child.UpdatedAt.After(*rollup.LastChildUpdateAt) {
			ts := child.UpdatedAt
			rollup.LastChildUpdateAt = &ts
		}
	}
	rollup.AllChildrenTerminal = rollup.TotalChildren > 0 && rollup.TerminalChildren == rollup.TotalChildren
	rollup.NeedsParentReconciliation = rollup.AllChildrenTerminal && !isTerminalTaskStatus(parent.Status)
	rollup.Summary = formatTaskRollupSummary(parent, rollup)
	return rollup
}

func formatTaskRollupSummary(parent *types.Task, rollup *types.TaskRollup) string {
	base := fmt.Sprintf(
		"Child rollup: total=%d active=%d completed=%d failed=%d cancelled=%d pending=%d in_progress=%d.",
		rollup.TotalChildren,
		rollup.ActiveChildren,
		rollup.CompletedChildren,
		rollup.FailedChildren,
		rollup.CancelledChildren,
		rollup.PendingChildren,
		rollup.InProgressChildren,
	)
	base = strings.TrimSuffix(base, ".") + fmt.Sprintf(" blocked=%d.", rollup.BlockedChildren)
	if rollup.NeedsParentReconciliation {
		return base + " All child tasks are terminal; parent reconciliation is still required."
	}
	if rollup.AllChildrenTerminal {
		return base + " All child tasks are terminal."
	}
	if parent.Status == types.TaskPending {
		return base + " Parent is waiting on child progress."
	}
	return base + " Parent remains open while child work is still active."
}

func shouldAppendRollupLog(task *types.Task, msg string) bool {
	if msg == "" || len(task.Logs) == 0 {
		return msg != ""
	}
	last := task.Logs[len(task.Logs)-1]
	return last.Message != msg
}

func taskRollupEqual(a, b *types.TaskRollup) bool {
	switch {
	case a == nil && b == nil:
		return true
	case a == nil || b == nil:
		return false
	}
	if a.TotalChildren != b.TotalChildren ||
		a.PendingChildren != b.PendingChildren ||
		a.InProgressChildren != b.InProgressChildren ||
		a.BlockedChildren != b.BlockedChildren ||
		a.CompletedChildren != b.CompletedChildren ||
		a.FailedChildren != b.FailedChildren ||
		a.CancelledChildren != b.CancelledChildren ||
		a.TerminalChildren != b.TerminalChildren ||
		a.ActiveChildren != b.ActiveChildren ||
		a.AllChildrenTerminal != b.AllChildrenTerminal ||
		a.NeedsParentReconciliation != b.NeedsParentReconciliation ||
		a.Summary != b.Summary {
		return false
	}
	switch {
	case a.LastChildUpdateAt == nil && b.LastChildUpdateAt == nil:
		return true
	case a.LastChildUpdateAt == nil || b.LastChildUpdateAt == nil:
		return false
	default:
		return a.LastChildUpdateAt.Equal(*b.LastChildUpdateAt)
	}
}

func validateTaskUpdate(task *types.Task, status *types.TaskStatus, result *string, logMsg *string, workspace string) error {
	if task.RemovedAt != nil {
		return fmt.Errorf("task %q has been removed", task.ID)
	}
	if workspace != task.Assignee && workspace != task.CreatedBy {
		return fmt.Errorf("workspace %q cannot update task %q", workspace, task.ID)
	}
	if result != nil && strings.TrimSpace(*result) != "" && workspace != task.Assignee {
		return fmt.Errorf("workspace %q cannot set result for task %q owned by %q", workspace, task.ID, task.Assignee)
	}
	if status == nil {
		return nil
	}
	if workspace != task.Assignee && *status != task.Status {
		return fmt.Errorf("workspace %q cannot change status for task %q owned by %q", workspace, task.ID, task.Assignee)
	}
	if !isAllowedTaskTransition(task.Status, *status) {
		return fmt.Errorf("invalid task status transition %q -> %q", task.Status, *status)
	}
	return nil
}

func isAllowedTaskTransition(current, next types.TaskStatus) bool {
	if current == next {
		return true
	}
	switch current {
	case types.TaskPending:
		return next == types.TaskInProgress || next == types.TaskBlocked || next == types.TaskCompleted || next == types.TaskFailed || next == types.TaskCancelled
	case types.TaskInProgress:
		return next == types.TaskBlocked || next == types.TaskCompleted || next == types.TaskFailed || next == types.TaskCancelled
	case types.TaskBlocked:
		return next == types.TaskPending || next == types.TaskInProgress || next == types.TaskCompleted || next == types.TaskFailed || next == types.TaskCancelled
	case types.TaskCompleted, types.TaskFailed, types.TaskCancelled:
		return false
	default:
		return false
	}
}

func validateTaskControl(task *types.Task, workspace string, expectedVersion *int64, allowAssignee bool) error {
	if task.RemovedAt != nil {
		return fmt.Errorf("task %q has been removed", task.ID)
	}
	if expectedVersion != nil && task.Version != *expectedVersion {
		return fmt.Errorf("task %q version mismatch: have %d want %d", task.ID, task.Version, *expectedVersion)
	}
	if workspace == operatorWorkspaceName || workspace == task.CreatedBy {
		return nil
	}
	if allowAssignee && workspace == task.Assignee {
		return nil
	}
	return fmt.Errorf("workspace %q cannot manage task %q", workspace, task.ID)
}

func isTerminalTaskStatus(status types.TaskStatus) bool {
	return status == types.TaskCompleted || status == types.TaskFailed || status == types.TaskCancelled
}

func isDuplicateTaskLog(task *types.Task, workspace, logMsg string, now time.Time) bool {
	normalized := normalizeMessageForSuppression(logMsg)
	if normalized == "" || !looksLikeNoOpStatusMessage(normalized) || len(task.Logs) == 0 {
		return false
	}
	last := task.Logs[len(task.Logs)-1]
	if last.Workspace != workspace {
		return false
	}
	if now.Sub(last.Timestamp) > duplicateSuppressionWindow {
		return false
	}
	return normalizeMessageForSuppression(last.Message) == normalized
}

func copyTask(task *types.Task) *types.Task {
	cp := *task
	cp.ChildTaskIDs = append([]string(nil), task.ChildTaskIDs...)
	cp.Logs = make([]types.TaskLog, len(task.Logs))
	copy(cp.Logs, task.Logs)
	cp.Rollup = copyTaskRollup(task.Rollup)
	cp.Sequence = copyTaskSequence(task.Sequence)
	return &cp
}

func clearDerivedTaskFields(task *types.Task) {
	if task == nil {
		return
	}
	task.Sequence = nil
	task.StaleInfo = nil
}

func copyTaskRollup(rollup *types.TaskRollup) *types.TaskRollup {
	if rollup == nil {
		return nil
	}
	cp := *rollup
	if rollup.LastChildUpdateAt != nil {
		ts := *rollup.LastChildUpdateAt
		cp.LastChildUpdateAt = &ts
	}
	return &cp
}

func copyTaskSequence(sequence *types.TaskSequenceInfo) *types.TaskSequenceInfo {
	if sequence == nil {
		return nil
	}
	cp := *sequence
	return &cp
}
