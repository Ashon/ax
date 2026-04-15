package daemon

import (
	"encoding/json"
	"fmt"

	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/usage"
)

type MessageType string

const (
	MsgRegister       MessageType = "register"
	MsgUnregister     MessageType = "unregister"
	MsgSendMessage    MessageType = "send_message"
	MsgBroadcast      MessageType = "broadcast"
	MsgReadMessages   MessageType = "read_messages"
	MsgListWorkspaces MessageType = "list_workspaces"
	MsgSetStatus      MessageType = "set_status"
	MsgSetShared      MessageType = "set_shared"
	MsgGetShared      MessageType = "get_shared"
	MsgListShared     MessageType = "list_shared"
	MsgUsageTrends    MessageType = "usage_trends"
	MsgCreateTask     MessageType = "create_task"
	MsgUpdateTask     MessageType = "update_task"
	MsgGetTask        MessageType = "get_task"
	MsgListTasks      MessageType = "list_tasks"
	MsgCancelTask     MessageType = "cancel_task"
	MsgRemoveTask     MessageType = "remove_task"
	MsgInterveneTask  MessageType = "intervene_task"
	MsgGetTeamState   MessageType = "get_team_state"
	MsgDryRunTeam     MessageType = "dry_run_team_reconfigure"
	MsgApplyTeam      MessageType = "apply_team_reconfigure"
	MsgFinishTeam     MessageType = "finish_team_reconfigure"
	MsgPushMessage    MessageType = "push_message"
	MsgResponse       MessageType = "response"
	MsgError          MessageType = "error"
)

// Envelope is the wire format for daemon <-> MCP server communication.
// Sent as newline-delimited JSON over Unix socket.
type Envelope struct {
	ID      string          `json:"id"`
	Type    MessageType     `json:"type"`
	Payload json.RawMessage `json:"payload"`
}

// Request payloads

type RegisterPayload struct {
	Workspace   string `json:"workspace"`
	Dir         string `json:"dir,omitempty"`
	Description string `json:"description,omitempty"`
}

type SendMessagePayload struct {
	To      string `json:"to"`
	Message string `json:"message"`
}

type BroadcastPayload struct {
	Message string `json:"message"`
}

type ReadMessagesPayload struct {
	Limit int    `json:"limit,omitempty"`
	From  string `json:"from,omitempty"`
}

type SetStatusPayload struct {
	Status string `json:"status"`
}

type SetSharedPayload struct {
	Key   string `json:"key"`
	Value string `json:"value"`
}

type GetSharedPayload struct {
	Key string `json:"key"`
}

type UsageTrendWorkspace struct {
	Workspace string `json:"workspace"`
	Cwd       string `json:"cwd"`
}

type UsageTrendsPayload struct {
	Workspaces    []UsageTrendWorkspace `json:"workspaces"`
	SinceMinutes  int                   `json:"since_minutes,omitempty"`
	BucketMinutes int                   `json:"bucket_minutes,omitempty"`
}

// Task payloads

type CreateTaskPayload struct {
	Title             string `json:"title"`
	Description       string `json:"description,omitempty"`
	Assignee          string `json:"assignee"`
	ParentTaskID      string `json:"parent_task_id,omitempty"`
	StartMode         string `json:"start_mode,omitempty"`
	Priority          string `json:"priority,omitempty"`
	StaleAfterSeconds int    `json:"stale_after_seconds,omitempty"`
}

type UpdateTaskPayload struct {
	ID     string            `json:"id"`
	Status *types.TaskStatus `json:"status,omitempty"`
	Result *string           `json:"result,omitempty"`
	Log    *string           `json:"log,omitempty"`
}

type GetTaskPayload struct {
	ID string `json:"id"`
}

type ListTasksPayload struct {
	Assignee  string            `json:"assignee,omitempty"`
	CreatedBy string            `json:"created_by,omitempty"`
	Status    *types.TaskStatus `json:"status,omitempty"`
}

type CancelTaskPayload struct {
	ID              string `json:"id"`
	Reason          string `json:"reason,omitempty"`
	ExpectedVersion *int64 `json:"expected_version,omitempty"`
}

type RemoveTaskPayload struct {
	ID              string `json:"id"`
	Reason          string `json:"reason,omitempty"`
	ExpectedVersion *int64 `json:"expected_version,omitempty"`
}

type InterveneTaskPayload struct {
	ID              string `json:"id"`
	Action          string `json:"action"`
	Note            string `json:"note,omitempty"`
	ExpectedVersion *int64 `json:"expected_version,omitempty"`
}

type GetTeamStatePayload struct {
	ConfigPath string `json:"config_path,omitempty"`
}

type TeamReconfigurePayload struct {
	ConfigPath       string                        `json:"config_path,omitempty"`
	ExpectedRevision *int                          `json:"expected_revision,omitempty"`
	Changes          []types.TeamReconfigureChange `json:"changes,omitempty"`
	ReconcileMode    types.TeamReconcileMode       `json:"reconcile_mode,omitempty"`
}

type FinishTeamReconfigurePayload struct {
	Token   string                        `json:"token"`
	Success bool                          `json:"success"`
	Error   string                        `json:"error,omitempty"`
	Actions []types.TeamReconfigureAction `json:"actions,omitempty"`
}

// Response payloads

type ResponsePayload struct {
	Success bool            `json:"success"`
	Data    json.RawMessage `json:"data,omitempty"`
}

type ErrorPayload struct {
	Message string `json:"message"`
}

type ListWorkspacesResponse struct {
	Workspaces []types.WorkspaceInfo `json:"workspaces"`
}

type ReadMessagesResponse struct {
	Messages []types.Message `json:"messages"`
}

type GetSharedResponse struct {
	Key   string `json:"key"`
	Value string `json:"value"`
	Found bool   `json:"found"`
}

type ListSharedResponse struct {
	Values map[string]string `json:"values"`
}

type UsageTrendsResponse struct {
	Trends []usage.WorkspaceTrend `json:"trends"`
}

// Task responses

type TaskResponse struct {
	Task types.Task `json:"task"`
}

type ListTasksResponse struct {
	Tasks []types.Task `json:"tasks"`
}

type InterveneTaskResponse struct {
	Task      types.Task `json:"task"`
	Action    string     `json:"action"`
	Status    string     `json:"status"`
	MessageID string     `json:"message_id,omitempty"`
}

type TeamStateResponse struct {
	State types.TeamReconfigureState `json:"state"`
}

type TeamPlanResponse struct {
	Plan types.TeamReconfigurePlan `json:"plan"`
}

type TeamApplyResponse struct {
	Ticket types.TeamApplyTicket `json:"ticket"`
}

// Helper functions

func NewEnvelope(id string, msgType MessageType, payload any) (*Envelope, error) {
	data, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("marshal payload: %w", err)
	}
	return &Envelope{
		ID:      id,
		Type:    msgType,
		Payload: data,
	}, nil
}

func (e *Envelope) DecodePayload(v any) error {
	return json.Unmarshal(e.Payload, v)
}

func NewResponseEnvelope(id string, data any) (*Envelope, error) {
	dataBytes, err := json.Marshal(data)
	if err != nil {
		return nil, fmt.Errorf("marshal response data: %w", err)
	}
	return NewEnvelope(id, MsgResponse, &ResponsePayload{
		Success: true,
		Data:    dataBytes,
	})
}

func NewErrorEnvelope(id string, message string) (*Envelope, error) {
	return NewEnvelope(id, MsgError, &ErrorPayload{Message: message})
}
