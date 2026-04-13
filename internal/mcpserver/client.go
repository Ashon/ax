package mcpserver

import (
	"bufio"
	"encoding/json"
	"fmt"
	"net"
	"sync"
	"sync/atomic"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/types"
	"github.com/google/uuid"
)

// DaemonClient connects to the ax daemon via Unix socket.
type DaemonClient struct {
	socketPath string
	workspace  string
	conn       net.Conn
	writeMu    sync.Mutex

	// Pending request tracking
	pending   map[string]chan *daemon.Envelope
	pendingMu sync.Mutex

	// Push message buffer
	pushMessages []types.Message
	pushMu       sync.Mutex

	connected atomic.Bool
}

func NewDaemonClient(socketPath, workspace string) *DaemonClient {
	return &DaemonClient{
		socketPath: daemon.ExpandSocketPath(socketPath),
		workspace:  workspace,
		pending:    make(map[string]chan *daemon.Envelope),
	}
}

func (c *DaemonClient) Connect() error {
	conn, err := net.Dial("unix", c.socketPath)
	if err != nil {
		return fmt.Errorf("connect to daemon: %w", err)
	}
	c.conn = conn
	c.connected.Store(true)

	// Start reader goroutine
	go c.readLoop()

	// Register with daemon
	_, err = c.sendRequest(daemon.MsgRegister, &daemon.RegisterPayload{
		Workspace: c.workspace,
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
				ch <- &env
				delete(c.pending, env.ID)
			}
			c.pendingMu.Unlock()
		}
	}

	c.connected.Store(false)
}

func (c *DaemonClient) sendRequest(msgType daemon.MessageType, payload any) (*daemon.Envelope, error) {
	id := uuid.New().String()
	env, err := daemon.NewEnvelope(id, msgType, payload)
	if err != nil {
		return nil, err
	}

	// Create response channel
	ch := make(chan *daemon.Envelope, 1)
	c.pendingMu.Lock()
	c.pending[id] = ch
	c.pendingMu.Unlock()

	// Send
	data, err := json.Marshal(env)
	if err != nil {
		return nil, err
	}
	data = append(data, '\n')

	c.writeMu.Lock()
	_, err = c.conn.Write(data)
	c.writeMu.Unlock()

	if err != nil {
		c.pendingMu.Lock()
		delete(c.pending, id)
		c.pendingMu.Unlock()
		return nil, fmt.Errorf("write: %w", err)
	}

	// Wait for response
	resp := <-ch
	if resp.Type == daemon.MsgError {
		var errPayload daemon.ErrorPayload
		resp.DecodePayload(&errPayload)
		return nil, fmt.Errorf("daemon error: %s", errPayload.Message)
	}

	return resp, nil
}

// High-level operations

func (c *DaemonClient) SendMessage(to, message string) (string, error) {
	resp, err := c.sendRequest(daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      to,
		Message: message,
	})
	if err != nil {
		return "", err
	}
	var result map[string]string
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	json.Unmarshal(respPayload.Data, &result)
	return result["message_id"], nil
}

func (c *DaemonClient) ReadMessages(limit int, from string) ([]types.Message, error) {
	resp, err := c.sendRequest(daemon.MsgReadMessages, &daemon.ReadMessagesPayload{
		Limit: limit,
		From:  from,
	})
	if err != nil {
		return nil, err
	}
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var result daemon.ReadMessagesResponse
	json.Unmarshal(respPayload.Data, &result)

	// Also include any pushed messages
	c.pushMu.Lock()
	pushed := c.pushMessages
	c.pushMessages = nil
	c.pushMu.Unlock()

	all := append(pushed, result.Messages...)
	return all, nil
}

func (c *DaemonClient) BroadcastMessage(message string) ([]string, error) {
	resp, err := c.sendRequest(daemon.MsgBroadcast, &daemon.BroadcastPayload{
		Message: message,
	})
	if err != nil {
		return nil, err
	}
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var result map[string]interface{}
	json.Unmarshal(respPayload.Data, &result)
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
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var result daemon.ListWorkspacesResponse
	json.Unmarshal(respPayload.Data, &result)
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
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var result daemon.GetSharedResponse
	json.Unmarshal(respPayload.Data, &result)
	return result.Value, result.Found, nil
}

func (c *DaemonClient) ListSharedValues() (map[string]string, error) {
	resp, err := c.sendRequest(daemon.MsgListShared, struct{}{})
	if err != nil {
		return nil, err
	}
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var result daemon.ListSharedResponse
	json.Unmarshal(respPayload.Data, &result)
	return result.Values, nil
}

// Task operations

func (c *DaemonClient) CreateTask(title, description, assignee string) (*types.Task, error) {
	resp, err := c.sendRequest(daemon.MsgCreateTask, &daemon.CreateTaskPayload{
		Title:       title,
		Description: description,
		Assignee:    assignee,
	})
	if err != nil {
		return nil, err
	}
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var result daemon.TaskResponse
	json.Unmarshal(respPayload.Data, &result)
	return &result.Task, nil
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
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var taskResp daemon.TaskResponse
	json.Unmarshal(respPayload.Data, &taskResp)
	return &taskResp.Task, nil
}

func (c *DaemonClient) GetTask(id string) (*types.Task, error) {
	resp, err := c.sendRequest(daemon.MsgGetTask, &daemon.GetTaskPayload{
		ID: id,
	})
	if err != nil {
		return nil, err
	}
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var result daemon.TaskResponse
	json.Unmarshal(respPayload.Data, &result)
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
	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)
	var result daemon.ListTasksResponse
	json.Unmarshal(respPayload.Data, &result)
	return result.Tasks, nil
}
