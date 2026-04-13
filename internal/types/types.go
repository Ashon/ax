package types

import "time"

type AgentStatus string

const (
	StatusOnline       AgentStatus = "online"
	StatusOffline      AgentStatus = "offline"
	StatusDisconnected AgentStatus = "disconnected"
)

type WorkspaceInfo struct {
	Name        string      `json:"name"`
	Dir         string      `json:"dir"`
	Description string      `json:"description,omitempty"`
	Status      AgentStatus `json:"status"`
	StatusText  string      `json:"status_text,omitempty"`
	ConnectedAt *time.Time  `json:"connected_at,omitempty"`
}

type Message struct {
	ID        string    `json:"id"`
	From      string    `json:"from"`
	To        string    `json:"to"`
	Content   string    `json:"content"`
	CreatedAt time.Time `json:"created_at"`
}

// Task management types

type TaskStatus string

const (
	TaskPending    TaskStatus = "pending"
	TaskInProgress TaskStatus = "in_progress"
	TaskCompleted  TaskStatus = "completed"
	TaskFailed     TaskStatus = "failed"
)

type Task struct {
	ID          string     `json:"id"`
	Title       string     `json:"title"`
	Description string     `json:"description,omitempty"`
	Assignee    string     `json:"assignee"`
	CreatedBy   string     `json:"created_by"`
	Status      TaskStatus `json:"status"`
	Result      string     `json:"result,omitempty"`
	Logs        []TaskLog  `json:"logs,omitempty"`
	CreatedAt   time.Time  `json:"created_at"`
	UpdatedAt   time.Time  `json:"updated_at"`
}

type TaskLog struct {
	Timestamp time.Time `json:"timestamp"`
	Workspace string    `json:"workspace"`
	Message   string    `json:"message"`
}
