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

type TaskStartMode string

const (
	TaskStartDefault TaskStartMode = "default"
	TaskStartFresh   TaskStartMode = "fresh"
)

type TaskPriority string

const (
	TaskPriorityLow    TaskPriority = "low"
	TaskPriorityNormal TaskPriority = "normal"
	TaskPriorityHigh   TaskPriority = "high"
	TaskPriorityUrgent TaskPriority = "urgent"
)

type Task struct {
	ID                string         `json:"id"`
	Title             string         `json:"title"`
	Description       string         `json:"description,omitempty"`
	Assignee          string         `json:"assignee"`
	CreatedBy         string         `json:"created_by"`
	Status            TaskStatus     `json:"status"`
	StartMode         TaskStartMode  `json:"start_mode"`
	Priority          TaskPriority   `json:"priority,omitempty"`
	StaleAfterSeconds int            `json:"stale_after_seconds,omitempty"`
	Result            string         `json:"result,omitempty"`
	Logs              []TaskLog      `json:"logs,omitempty"`
	StaleInfo         *TaskStaleInfo `json:"stale_info,omitempty"`
	CreatedAt         time.Time      `json:"created_at"`
	UpdatedAt         time.Time      `json:"updated_at"`
}

type TaskLog struct {
	Timestamp time.Time `json:"timestamp"`
	Workspace string    `json:"workspace"`
	Message   string    `json:"message"`
}

type TaskStaleInfo struct {
	IsStale             bool       `json:"is_stale"`
	Reason              string     `json:"reason,omitempty"`
	RecommendedAction   string     `json:"recommended_action,omitempty"`
	LastProgressAt      time.Time  `json:"last_progress_at"`
	AgeSeconds          int64      `json:"age_seconds"`
	PendingMessages     int        `json:"pending_messages"`
	LastMessageAt       *time.Time `json:"last_message_at,omitempty"`
	WakePending         bool       `json:"wake_pending,omitempty"`
	WakeAttempts        int        `json:"wake_attempts,omitempty"`
	NextWakeRetryAt     *time.Time `json:"next_wake_retry_at,omitempty"`
	StateDivergence     bool       `json:"state_divergence,omitempty"`
	StateDivergenceNote string     `json:"state_divergence_note,omitempty"`
}
