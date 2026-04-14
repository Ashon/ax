package daemon

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/ashon/ax/internal/types"
	"github.com/google/uuid"
)

type TaskStore struct {
	mu       sync.RWMutex
	tasks    map[string]*types.Task
	filePath string
}

func NewTaskStore(stateDir string) *TaskStore {
	return &TaskStore{
		tasks:    make(map[string]*types.Task),
		filePath: filepath.Join(stateDir, "tasks.json"),
	}
}

func (s *TaskStore) Load() error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.filePath == "" {
		return nil
	}
	data, err := os.ReadFile(s.filePath)
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
		cp := task
		cp.Logs = make([]types.TaskLog, len(task.Logs))
		copy(cp.Logs, task.Logs)
		loaded[task.ID] = &cp
	}
	s.tasks = loaded
	return nil
}

// TasksFilePath returns the path to the tasks file for external readers (watch).
func TasksFilePath(socketPath string) string {
	return filepath.Join(filepath.Dir(ExpandSocketPath(socketPath)), "tasks.json")
}

func (s *TaskStore) Create(title, description, assignee, createdBy string, startMode types.TaskStartMode, priority types.TaskPriority, staleAfterSeconds int) *types.Task {
	s.mu.Lock()
	defer s.mu.Unlock()

	if startMode == "" {
		startMode = types.TaskStartDefault
	}
	if priority == "" {
		priority = types.TaskPriorityNormal
	}

	now := time.Now()
	task := &types.Task{
		ID:                uuid.New().String(),
		Title:             title,
		Description:       description,
		Assignee:          assignee,
		CreatedBy:         createdBy,
		Status:            types.TaskPending,
		StartMode:         startMode,
		Priority:          priority,
		StaleAfterSeconds: staleAfterSeconds,
		CreatedAt:         now,
		UpdatedAt:         now,
	}
	s.tasks[task.ID] = task
	s.persist()
	return task
}

func (s *TaskStore) Get(id string) (*types.Task, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	task, ok := s.tasks[id]
	if !ok {
		return nil, false
	}
	cp := *task
	cp.Logs = make([]types.TaskLog, len(task.Logs))
	copy(cp.Logs, task.Logs)
	return &cp, true
}

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
	if status != nil {
		if task.Status != *status {
			task.Status = *status
			changed = true
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
		task.UpdatedAt = now
		s.persist()
	}

	cp := *task
	cp.Logs = make([]types.TaskLog, len(task.Logs))
	copy(cp.Logs, task.Logs)
	return &cp, nil
}

func (s *TaskStore) List(assignee, createdBy string, status *types.TaskStatus) []types.Task {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make([]types.Task, 0, len(s.tasks))
	for _, task := range s.tasks {
		if assignee != "" && task.Assignee != assignee {
			continue
		}
		if createdBy != "" && task.CreatedBy != createdBy {
			continue
		}
		if status != nil && task.Status != *status {
			continue
		}
		cp := *task
		cp.Logs = make([]types.TaskLog, len(task.Logs))
		copy(cp.Logs, task.Logs)
		result = append(result, cp)
	}
	return result
}

func (s *TaskStore) persist() {
	if s.filePath == "" {
		return
	}
	tasks := make([]types.Task, 0, len(s.tasks))
	for _, t := range s.tasks {
		tasks = append(tasks, *t)
	}
	data, err := json.Marshal(tasks)
	if err != nil {
		return
	}
	os.WriteFile(s.filePath, data, 0o644)
}

func (s *TaskStore) Snapshot() []types.Task {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make([]types.Task, 0, len(s.tasks))
	for _, task := range s.tasks {
		cp := *task
		cp.Logs = make([]types.TaskLog, len(task.Logs))
		copy(cp.Logs, task.Logs)
		result = append(result, cp)
	}
	return result
}

func (s *TaskStore) Refresh(transform func(types.Task) types.Task) {
	s.mu.Lock()
	defer s.mu.Unlock()

	for id, task := range s.tasks {
		s.tasks[id] = taskFromSnapshot(transform(snapshotTask(task)))
	}
	s.persist()
}

func snapshotTask(task *types.Task) types.Task {
	cp := *task
	cp.Logs = make([]types.TaskLog, len(task.Logs))
	copy(cp.Logs, task.Logs)
	return cp
}

func taskFromSnapshot(task types.Task) *types.Task {
	cp := task
	cp.Logs = make([]types.TaskLog, len(task.Logs))
	copy(cp.Logs, task.Logs)
	return &cp
}

func validateTaskUpdate(task *types.Task, status *types.TaskStatus, result *string, logMsg *string, workspace string) error {
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
		return next == types.TaskInProgress || next == types.TaskCompleted || next == types.TaskFailed
	case types.TaskInProgress:
		return next == types.TaskCompleted || next == types.TaskFailed
	case types.TaskCompleted, types.TaskFailed:
		return false
	default:
		return false
	}
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
