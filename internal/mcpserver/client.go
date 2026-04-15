package mcpserver

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"sort"
	"sync"
	"sync/atomic"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/usage"
	"github.com/google/uuid"
)

// DefaultRequestTimeout bounds how long a single daemon request may wait
// for a response before being abandoned. Without this, a daemon that
// stays connected but stops answering a particular request type would
// hang the calling MCP tool forever.
const DefaultRequestTimeout = 60 * time.Second

// DaemonClient connects to the ax daemon via Unix socket.
type DaemonClient struct {
	socketPath  string
	workspace   string
	dir         string
	description string
	conn        net.Conn
	writeMu     sync.Mutex

	// Pending request tracking
	pending   map[string]chan requestResult
	pendingMu sync.Mutex

	// Push message buffer
	pushMessages []types.Message
	pushMu       sync.Mutex

	connected atomic.Bool

	disconnectMu  sync.RWMutex
	disconnectErr error

	// Default per-request timeout when callers do not supply their own
	// deadline. Overridable in tests.
	requestTimeout time.Duration
}

type requestResult struct {
	env *daemon.Envelope
	err error
}

func NewDaemonClient(socketPath, workspace string) *DaemonClient {
	return &DaemonClient{
		socketPath:     daemon.ExpandSocketPath(socketPath),
		workspace:      workspace,
		pending:        make(map[string]chan requestResult),
		requestTimeout: DefaultRequestTimeout,
	}
}

// SetRequestTimeout overrides the default per-request timeout. A
// non-positive value disables the bound. Intended for tests.
func (c *DaemonClient) SetRequestTimeout(d time.Duration) {
	c.requestTimeout = d
}

func (c *DaemonClient) SetRegistrationInfo(dir, description string) {
	c.dir = dir
	c.description = description
}

func (c *DaemonClient) Connect() error {
	conn, err := net.Dial("unix", c.socketPath)
	if err != nil {
		return fmt.Errorf("connect to daemon: %w", err)
	}
	c.conn = conn
	c.setDisconnectErr(nil)
	c.connected.Store(true)

	// Start reader goroutine
	go c.readLoop()

	dir := c.dir
	if dir == "" {
		if wd, err := os.Getwd(); err == nil {
			dir = wd
		}
	}

	// Register with daemon
	_, err = c.sendRequest(daemon.MsgRegister, &daemon.RegisterPayload{
		Workspace:   c.workspace,
		Dir:         dir,
		Description: c.description,
	})
	if err != nil {
		conn.Close()
		c.connected.Store(false)
		return fmt.Errorf("register: %w", err)
	}

	return nil
}

func (c *DaemonClient) Close() error {
	c.connected.Store(false)
	if c.conn != nil {
		return c.conn.Close()
	}
	return nil
}

func (c *DaemonClient) readLoop() {
	scanner := bufio.NewScanner(c.conn)
	scanner.Buffer(make([]byte, 1024*1024), 1024*1024)

	for scanner.Scan() {
		var env daemon.Envelope
		if err := json.Unmarshal(scanner.Bytes(), &env); err != nil {
			continue
		}

		switch env.Type {
		case daemon.MsgPushMessage:
			// Pushed message from another agent
			var msg types.Message
			if err := env.DecodePayload(&msg); err == nil {
				c.pushMu.Lock()
				c.pushMessages = append(c.pushMessages, msg)
				c.pushMu.Unlock()
			}
		case daemon.MsgResponse, daemon.MsgError:
			// Response to a pending request
			c.pendingMu.Lock()
			if ch, ok := c.pending[env.ID]; ok {
				ch <- requestResult{env: &env}
				delete(c.pending, env.ID)
			}
			c.pendingMu.Unlock()
		}
	}

	c.markDisconnected(scanner.Err())
}

func (c *DaemonClient) sendRequest(msgType daemon.MessageType, payload any) (*daemon.Envelope, error) {
	return c.sendRequestCtx(context.Background(), msgType, payload)
}

// sendRequestCtx is the context-aware variant of sendRequest. It applies
// the client's default timeout when the supplied context has no deadline
// and unblocks the caller as soon as the context is cancelled, even if
// the daemon has not yet returned a response. Pending entries are always
// cleaned up so a cancelled or timed-out request never leaks the
// response channel.
func (c *DaemonClient) sendRequestCtx(ctx context.Context, msgType daemon.MessageType, payload any) (*daemon.Envelope, error) {
	if !c.connected.Load() {
		return nil, c.disconnectError()
	}

	if ctx == nil {
		ctx = context.Background()
	}
	if _, hasDeadline := ctx.Deadline(); !hasDeadline && c.requestTimeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, c.requestTimeout)
		defer cancel()
	}

	id := uuid.New().String()
	env, err := daemon.NewEnvelope(id, msgType, payload)
	if err != nil {
		return nil, err
	}

	// Create response channel
	ch := make(chan requestResult, 1)
	c.pendingMu.Lock()
	c.pending[id] = ch
	c.pendingMu.Unlock()
	cleanupPending := func() {
		c.pendingMu.Lock()
		delete(c.pending, id)
		c.pendingMu.Unlock()
	}

	// Send
	data, err := json.Marshal(env)
	if err != nil {
		cleanupPending()
		return nil, err
	}
	data = append(data, '\n')

	c.writeMu.Lock()
	_, err = c.conn.Write(data)
	c.writeMu.Unlock()

	if err != nil {
		cleanupPending()
		return nil, fmt.Errorf("write: %w", err)
	}

	// Wait for response, the connection going away, or the context
	// deadline elapsing.
	var result requestResult
	select {
	case result = <-ch:
	case <-ctx.Done():
		cleanupPending()
		return nil, fmt.Errorf("daemon request %s: %w", msgType, ctx.Err())
	}
	if result.err != nil {
		return nil, result.err
	}
	resp := result.env
	if resp.Type == daemon.MsgError {
		var errPayload daemon.ErrorPayload
		_ = resp.DecodePayload(&errPayload)
		return nil, fmt.Errorf("daemon error: %s", errPayload.Message)
	}

	return resp, nil
}

func (c *DaemonClient) markDisconnected(err error) {
	c.connected.Store(false)
	disconnectErr := normalizeDisconnectError(err)
	c.setDisconnectErr(disconnectErr)

	c.pendingMu.Lock()
	pending := c.pending
	c.pending = make(map[string]chan requestResult)
	c.pendingMu.Unlock()

	for _, ch := range pending {
		ch <- requestResult{err: disconnectErr}
	}
}

func normalizeDisconnectError(err error) error {
	if err == nil {
		err = io.EOF
	}
	return fmt.Errorf("daemon connection closed: %w", err)
}

func (c *DaemonClient) disconnectError() error {
	c.disconnectMu.RLock()
	defer c.disconnectMu.RUnlock()
	if c.disconnectErr != nil {
		return c.disconnectErr
	}
	return normalizeDisconnectError(nil)
}

func (c *DaemonClient) setDisconnectErr(err error) {
	c.disconnectMu.Lock()
	defer c.disconnectMu.Unlock()
	c.disconnectErr = err
}

// High-level operations

type SendMessageResult struct {
	MessageID  string
	Status     string
	Suppressed bool
}

func (c *DaemonClient) SendMessage(to, message string) (*SendMessageResult, error) {
	resp, err := c.sendRequest(daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      to,
		Message: message,
	})
	if err != nil {
		return nil, err
	}
	var result map[string]string
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode send_message response: %w", err)
	}
	sendResult := &SendMessageResult{
		MessageID: result["message_id"],
		Status:    result["status"],
	}
	sendResult.Suppressed = sendResult.Status == "suppressed"
	return sendResult, nil
}

func (c *DaemonClient) ReadMessages(limit int, from string) ([]types.Message, error) {
	resp, err := c.sendRequest(daemon.MsgReadMessages, &daemon.ReadMessagesPayload{
		Limit: limit,
		From:  from,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.ReadMessagesResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode read_messages response: %w", err)
	}

	// Also include any pushed messages while preserving unmatched buffered pushes
	// for later calls and avoiding duplicate delivery of the same message ID.
	c.pushMu.Lock()
	pushed, remaining := splitBufferedMessages(c.pushMessages, from)
	c.pushMessages = remaining
	c.pushMu.Unlock()

	return mergeUniqueMessages(pushed, result.Messages), nil
}

func splitBufferedMessages(messages []types.Message, from string) ([]types.Message, []types.Message) {
	if len(messages) == 0 {
		return nil, nil
	}
	matched := make([]types.Message, 0, len(messages))
	remaining := make([]types.Message, 0, len(messages))
	for _, msg := range messages {
		if from != "" && msg.From != from {
			remaining = append(remaining, msg)
			continue
		}
		matched = append(matched, msg)
	}
	return matched, remaining
}

func mergeUniqueMessages(first, second []types.Message) []types.Message {
	if len(first) == 0 && len(second) == 0 {
		return nil
	}
	seen := make(map[string]struct{}, len(first)+len(second))
	merged := make([]types.Message, 0, len(first)+len(second))
	appendUnique := func(messages []types.Message) {
		for _, msg := range messages {
			if msg.ID != "" {
				if _, ok := seen[msg.ID]; ok {
					continue
				}
				seen[msg.ID] = struct{}{}
			}
			merged = append(merged, msg)
		}
	}
	appendUnique(first)
	appendUnique(second)

	sort.SliceStable(merged, func(i, j int) bool {
		if merged[i].CreatedAt.Equal(merged[j].CreatedAt) {
			return merged[i].ID < merged[j].ID
		}
		return merged[i].CreatedAt.Before(merged[j].CreatedAt)
	})
	return merged
}

func (c *DaemonClient) BroadcastMessage(message string) ([]string, error) {
	resp, err := c.sendRequest(daemon.MsgBroadcast, &daemon.BroadcastPayload{
		Message: message,
	})
	if err != nil {
		return nil, err
	}
	var result map[string]interface{}
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode broadcast response: %w", err)
	}
	recipients, _ := result["recipients"].([]interface{})
	var names []string
	for _, r := range recipients {
		if s, ok := r.(string); ok {
			names = append(names, s)
		}
	}
	return names, nil
}

func (c *DaemonClient) ListWorkspaces() ([]types.WorkspaceInfo, error) {
	resp, err := c.sendRequest(daemon.MsgListWorkspaces, struct{}{})
	if err != nil {
		return nil, err
	}
	var result daemon.ListWorkspacesResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode list_workspaces response: %w", err)
	}
	return result.Workspaces, nil
}

func (c *DaemonClient) SetStatus(status string) error {
	_, err := c.sendRequest(daemon.MsgSetStatus, &daemon.SetStatusPayload{
		Status: status,
	})
	return err
}

func (c *DaemonClient) SetSharedValue(key, value string) error {
	_, err := c.sendRequest(daemon.MsgSetShared, &daemon.SetSharedPayload{
		Key:   key,
		Value: value,
	})
	return err
}

func (c *DaemonClient) GetSharedValue(key string) (string, bool, error) {
	resp, err := c.sendRequest(daemon.MsgGetShared, &daemon.GetSharedPayload{
		Key: key,
	})
	if err != nil {
		return "", false, err
	}
	var result daemon.GetSharedResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return "", false, fmt.Errorf("decode get_shared response: %w", err)
	}
	return result.Value, result.Found, nil
}

func (c *DaemonClient) ListSharedValues() (map[string]string, error) {
	resp, err := c.sendRequest(daemon.MsgListShared, struct{}{})
	if err != nil {
		return nil, err
	}
	var result daemon.ListSharedResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode list_shared response: %w", err)
	}
	return result.Values, nil
}

func (c *DaemonClient) GetUsageTrends(workspaces []daemon.UsageTrendWorkspace, sinceMinutes, bucketMinutes int) ([]usage.WorkspaceTrend, error) {
	resp, err := c.sendRequest(daemon.MsgUsageTrends, &daemon.UsageTrendsPayload{
		Workspaces:    workspaces,
		SinceMinutes:  sinceMinutes,
		BucketMinutes: bucketMinutes,
	})
	if err != nil {
		return nil, err
	}
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var result daemon.UsageTrendsResponse
	json.Unmarshal(respPayload.Data, &result)
	return result.Trends, nil
}

// Task operations

func (c *DaemonClient) CreateTask(title, description, assignee, parentTaskID string, startMode types.TaskStartMode, workflowMode types.TaskWorkflowMode, priority types.TaskPriority, staleAfterSeconds int) (*types.Task, error) {
	resp, err := c.sendRequest(daemon.MsgCreateTask, &daemon.CreateTaskPayload{
		Title:             title,
		Description:       description,
		Assignee:          assignee,
		ParentTaskID:      parentTaskID,
		StartMode:         string(startMode),
		WorkflowMode:      string(workflowMode),
		Priority:          string(priority),
		StaleAfterSeconds: staleAfterSeconds,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.TaskResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode create_task response: %w", err)
	}
	return &result.Task, nil
}

func (c *DaemonClient) StartTask(title, description, message, assignee, parentTaskID string, startMode types.TaskStartMode, workflowMode types.TaskWorkflowMode, priority types.TaskPriority, staleAfterSeconds int) (*daemon.StartTaskResponse, error) {
	resp, err := c.sendRequest(daemon.MsgStartTask, &daemon.StartTaskPayload{
		Title:             title,
		Description:       description,
		Message:           message,
		Assignee:          assignee,
		ParentTaskID:      parentTaskID,
		StartMode:         string(startMode),
		WorkflowMode:      string(workflowMode),
		Priority:          string(priority),
		StaleAfterSeconds: staleAfterSeconds,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.StartTaskResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode start_task response: %w", err)
	}
	return &result, nil
}

func (c *DaemonClient) UpdateTask(id string, status *types.TaskStatus, result *string, logMsg *string) (*types.Task, error) {
	resp, err := c.sendRequest(daemon.MsgUpdateTask, &daemon.UpdateTaskPayload{
		ID:     id,
		Status: status,
		Result: result,
		Log:    logMsg,
	})
	if err != nil {
		return nil, err
	}
	var taskResp daemon.TaskResponse
	if err := decodeResponseData(resp, &taskResp); err != nil {
		return nil, fmt.Errorf("decode update_task response: %w", err)
	}
	return &taskResp.Task, nil
}

func (c *DaemonClient) GetTask(id string) (*types.Task, error) {
	resp, err := c.sendRequest(daemon.MsgGetTask, &daemon.GetTaskPayload{
		ID: id,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.TaskResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode get_task response: %w", err)
	}
	return &result.Task, nil
}

func (c *DaemonClient) ListTasks(assignee, createdBy string, status *types.TaskStatus) ([]types.Task, error) {
	resp, err := c.sendRequest(daemon.MsgListTasks, &daemon.ListTasksPayload{
		Assignee:  assignee,
		CreatedBy: createdBy,
		Status:    status,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.ListTasksResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode list_tasks response: %w", err)
	}
	return result.Tasks, nil
}

func (c *DaemonClient) CancelTask(id, reason string, expectedVersion *int64) (*types.Task, error) {
	resp, err := c.sendRequest(daemon.MsgCancelTask, &daemon.CancelTaskPayload{
		ID:              id,
		Reason:          reason,
		ExpectedVersion: expectedVersion,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.TaskResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode cancel_task response: %w", err)
	}
	return &result.Task, nil
}

func (c *DaemonClient) RemoveTask(id, reason string, expectedVersion *int64) (*types.Task, error) {
	resp, err := c.sendRequest(daemon.MsgRemoveTask, &daemon.RemoveTaskPayload{
		ID:              id,
		Reason:          reason,
		ExpectedVersion: expectedVersion,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.TaskResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode remove_task response: %w", err)
	}
	return &result.Task, nil
}

func (c *DaemonClient) InterveneTask(id, action, note string, expectedVersion *int64) (*daemon.InterveneTaskResponse, error) {
	resp, err := c.sendRequest(daemon.MsgInterveneTask, &daemon.InterveneTaskPayload{
		ID:              id,
		Action:          action,
		Note:            note,
		ExpectedVersion: expectedVersion,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.InterveneTaskResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode intervene_task response: %w", err)
	}
	return &result, nil
}

func (c *DaemonClient) GetTeamState(configPath string) (*types.TeamReconfigureState, error) {
	resp, err := c.sendRequest(daemon.MsgGetTeamState, &daemon.GetTeamStatePayload{
		ConfigPath: configPath,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.TeamStateResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode get_team_state response: %w", err)
	}
	return &result.State, nil
}

func (c *DaemonClient) DryRunTeamReconfigure(configPath string, expectedRevision *int, changes []types.TeamReconfigureChange) (*types.TeamReconfigurePlan, error) {
	resp, err := c.sendRequest(daemon.MsgDryRunTeam, &daemon.TeamReconfigurePayload{
		ConfigPath:       configPath,
		ExpectedRevision: expectedRevision,
		Changes:          changes,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.TeamPlanResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode dry_run_team_reconfigure response: %w", err)
	}
	return &result.Plan, nil
}

func (c *DaemonClient) ApplyTeamReconfigure(configPath string, expectedRevision *int, changes []types.TeamReconfigureChange, mode types.TeamReconcileMode) (*types.TeamApplyTicket, error) {
	resp, err := c.sendRequest(daemon.MsgApplyTeam, &daemon.TeamReconfigurePayload{
		ConfigPath:       configPath,
		ExpectedRevision: expectedRevision,
		Changes:          changes,
		ReconcileMode:    mode,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.TeamApplyResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode apply_team_reconfigure response: %w", err)
	}
	return &result.Ticket, nil
}

func (c *DaemonClient) FinishTeamReconfigure(token string, success bool, errText string, actions []types.TeamReconfigureAction) (*types.TeamReconfigureState, error) {
	resp, err := c.sendRequest(daemon.MsgFinishTeam, &daemon.FinishTeamReconfigurePayload{
		Token:   token,
		Success: success,
		Error:   errText,
		Actions: actions,
	})
	if err != nil {
		return nil, err
	}
	var result daemon.TeamStateResponse
	if err := decodeResponseData(resp, &result); err != nil {
		return nil, fmt.Errorf("decode finish_team_reconfigure response: %w", err)
	}
	return &result.State, nil
}

// decodeResponseData unwraps a daemon response envelope and decodes its
// payload into the supplied destination. It surfaces decode errors that
// were previously silently dropped, so MCP tools can fail loudly when the
// daemon returns an unexpected payload shape.
func decodeResponseData(env *daemon.Envelope, dst any) error {
	if env == nil {
		return fmt.Errorf("nil response envelope")
	}
	var respPayload daemon.ResponsePayload
	if err := env.DecodePayload(&respPayload); err != nil {
		return fmt.Errorf("decode response envelope: %w", err)
	}
	if len(respPayload.Data) == 0 {
		return nil
	}
	if err := json.Unmarshal(respPayload.Data, dst); err != nil {
		return fmt.Errorf("unmarshal response data: %w", err)
	}
	return nil
}
