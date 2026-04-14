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

const ExperimentalMCPTeamReconfigureFlagKey = "experimental_mcp_team_reconfigure"

type TeamEntryKind string

const (
	TeamEntryWorkspace        TeamEntryKind = "workspace"
	TeamEntryChild            TeamEntryKind = "child"
	TeamEntryRootOrchestrator TeamEntryKind = "root_orchestrator"
)

type TeamChangeOp string

const (
	TeamChangeAdd     TeamChangeOp = "add"
	TeamChangeRemove  TeamChangeOp = "remove"
	TeamChangeEnable  TeamChangeOp = "enable"
	TeamChangeDisable TeamChangeOp = "disable"
)

type TeamReconcileMode string

const (
	TeamReconcileArtifactsOnly TeamReconcileMode = "artifacts_only"
	TeamReconcileStartMissing  TeamReconcileMode = "start_missing"
)

type TeamWorkspaceSpec struct {
	Dir                       string            `json:"dir"`
	Description               string            `json:"description,omitempty"`
	Shell                     string            `json:"shell,omitempty"`
	Runtime                   string            `json:"runtime,omitempty"`
	CodexModelReasoningEffort string            `json:"codex_model_reasoning_effort,omitempty"`
	Agent                     string            `json:"agent,omitempty"`
	Instructions              string            `json:"instructions,omitempty"`
	Env                       map[string]string `json:"env,omitempty"`
}

type TeamChildSpec struct {
	Dir    string `json:"dir"`
	Prefix string `json:"prefix,omitempty"`
}

type TeamReconfigureChange struct {
	Op        TeamChangeOp       `json:"op"`
	Kind      TeamEntryKind      `json:"kind"`
	Name      string             `json:"name,omitempty"`
	Workspace *TeamWorkspaceSpec `json:"workspace,omitempty"`
	Child     *TeamChildSpec     `json:"child,omitempty"`
}

type TeamOverlay struct {
	DisableRootOrchestrator *bool                         `json:"disable_root_orchestrator,omitempty"`
	AddedWorkspaces         map[string]TeamWorkspaceSpec  `json:"added_workspaces,omitempty"`
	RemovedWorkspaces       map[string]bool               `json:"removed_workspaces,omitempty"`
	DisabledWorkspaces      map[string]bool               `json:"disabled_workspaces,omitempty"`
	AddedChildren           map[string]TeamChildSpec      `json:"added_children,omitempty"`
	RemovedChildren         map[string]bool               `json:"removed_children,omitempty"`
	DisabledChildren        map[string]bool               `json:"disabled_children,omitempty"`
}

type TeamConfiguredState struct {
	RootOrchestratorEnabled bool     `json:"root_orchestrator_enabled"`
	Workspaces              []string `json:"workspaces,omitempty"`
	Children                []string `json:"children,omitempty"`
	Orchestrators           []string `json:"orchestrators,omitempty"`
}

type TeamReconfigureAction struct {
	Action string        `json:"action"`
	Kind   TeamEntryKind `json:"kind"`
	Name   string        `json:"name,omitempty"`
	Dir    string        `json:"dir,omitempty"`
	Detail string        `json:"detail,omitempty"`
}

type TeamApplyReport struct {
	StartedAt     time.Time               `json:"started_at"`
	FinishedAt    *time.Time              `json:"finished_at,omitempty"`
	Success       bool                    `json:"success"`
	Error         string                  `json:"error,omitempty"`
	ReconcileMode TeamReconcileMode       `json:"reconcile_mode,omitempty"`
	Actions       []TeamReconfigureAction `json:"actions,omitempty"`
}

type TeamReconfigureState struct {
	TeamID              string               `json:"team_id"`
	BaseConfigPath      string               `json:"base_config_path"`
	EffectiveConfigPath string               `json:"effective_config_path"`
	FeatureEnabled      bool                 `json:"feature_enabled"`
	Revision            int                  `json:"revision"`
	Overlay             TeamOverlay          `json:"overlay,omitempty"`
	Desired             TeamConfiguredState  `json:"desired"`
	LastApply           *TeamApplyReport     `json:"last_apply,omitempty"`
}

type TeamReconfigurePlan struct {
	State            TeamReconfigureState    `json:"state"`
	ExpectedRevision int                     `json:"expected_revision"`
	Changes          []TeamReconfigureChange `json:"changes"`
	Actions          []TeamReconfigureAction `json:"actions,omitempty"`
	Warnings         []string                `json:"warnings,omitempty"`
}

type TeamApplyTicket struct {
	Token         string               `json:"token"`
	Plan          TeamReconfigurePlan  `json:"plan"`
	ReconcileMode TeamReconcileMode    `json:"reconcile_mode"`
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
