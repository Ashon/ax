package daemon

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
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

// TasksFilePath returns the path to the tasks file for external readers (watch).
func TasksFilePath(socketPath string) string {
	return filepath.Join(filepath.Dir(ExpandSocketPath(socketPath)), "tasks.json")
}

func (s *TaskStore) Create(title, description, assignee, createdBy string) *types.Task {
	s.mu.Lock()
	defer s.mu.Unlock()

	now := time.Now()
	task := &types.Task{
		ID:          uuid.New().String(),
		Title:       title,
		Description: description,
		Assignee:    assignee,
		CreatedBy:   createdBy,
		Status:      types.TaskPending,
		CreatedAt:   now,
		UpdatedAt:   now,
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

	now := time.Now()
	if status != nil {
		task.Status = *status
	}
	if result != nil {
		task.Result = *result
	}
	if logMsg != nil {
		task.Logs = append(task.Logs, types.TaskLog{
			Timestamp: now,
			Workspace: workspace,
			Message:   *logMsg,
		})
	}
	task.UpdatedAt = now
	s.persist()

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
