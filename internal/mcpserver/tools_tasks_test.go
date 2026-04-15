package mcpserver

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/types"
	"github.com/mark3labs/mcp-go/mcp"
)

func TestStartTaskHandlerCreatesAndDispatchesTaskAwareMessage(t *testing.T) {
	restoreWake := stubWakeWorkspaceAgent(func(target, sender string, fresh bool) {})
	defer restoreWake()

	const taskID = "11111111-1111-1111-1111-111111111111"

	client, serverErr := newTaskToolTestClient(t, 2, func(step int, env *daemon.Envelope) (*daemon.Envelope, error) {
		switch step {
		case 0:
			if env.Type != daemon.MsgCreateTask {
				return nil, fmt.Errorf("step 0 request type = %s, want %s", env.Type, daemon.MsgCreateTask)
			}
			var payload daemon.CreateTaskPayload
			if err := env.DecodePayload(&payload); err != nil {
				return nil, err
			}
			if payload.Title != "Investigate flaky task start" {
				return nil, fmt.Errorf("unexpected title %q", payload.Title)
			}
			if payload.Description != "Track the fresh-start handoff" {
				return nil, fmt.Errorf("unexpected description %q", payload.Description)
			}
			if payload.Assignee != "worker" {
				return nil, fmt.Errorf("unexpected assignee %q", payload.Assignee)
			}
			if payload.StartMode != string(types.TaskStartDefault) {
				return nil, fmt.Errorf("unexpected start mode %q", payload.StartMode)
			}
			if payload.Priority != string(types.TaskPriorityHigh) {
				return nil, fmt.Errorf("unexpected priority %q", payload.Priority)
			}
			if payload.StaleAfterSeconds != 300 {
				return nil, fmt.Errorf("unexpected stale_after_seconds %d", payload.StaleAfterSeconds)
			}
			return daemon.NewResponseEnvelope(env.ID, &daemon.TaskResponse{
				Task: types.Task{
					ID:          taskID,
					Title:       payload.Title,
					Description: payload.Description,
					Assignee:    payload.Assignee,
					CreatedBy:   "tester",
					Status:      types.TaskPending,
					StartMode:   types.TaskStartDefault,
					Priority:    types.TaskPriorityHigh,
				},
			})
		case 1:
			if env.Type != daemon.MsgSendMessage {
				return nil, fmt.Errorf("step 1 request type = %s, want %s", env.Type, daemon.MsgSendMessage)
			}
			var payload daemon.SendMessagePayload
			if err := env.DecodePayload(&payload); err != nil {
				return nil, err
			}
			if payload.To != "worker" {
				return nil, fmt.Errorf("unexpected message target %q", payload.To)
			}
			wantMessage := "Task ID: " + taskID + "\n\nInspect the fresh start handoff"
			if payload.Message != wantMessage {
				return nil, fmt.Errorf("unexpected dispatch message %q", payload.Message)
			}
			return daemon.NewResponseEnvelope(env.ID, map[string]string{
				"message_id": "msg-1",
				"status":     "sent",
			})
		default:
			return nil, fmt.Errorf("unexpected request step %d", step)
		}
	})

	result, err := startTaskHandler(client, "")(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"title":               "Investigate flaky task start",
				"description":         "Track the fresh-start handoff",
				"assignee":            "worker",
				"message":             "Inspect the fresh start handoff",
				"priority":            "high",
				"stale_after_seconds": 300,
			},
		},
	})
	if err != nil {
		t.Fatalf("startTaskHandler returned error: %v", err)
	}
	if err := <-serverErr; err != nil {
		t.Fatalf("daemon stub failed: %v", err)
	}

	var payload startTaskResult
	decodeToolResultJSON(t, result, &payload)
	if payload.Task.ID != taskID {
		t.Fatalf("task id = %q, want %q", payload.Task.ID, taskID)
	}
	if payload.Dispatch.MessageID != "msg-1" {
		t.Fatalf("dispatch message_id = %q, want msg-1", payload.Dispatch.MessageID)
	}
	if payload.Dispatch.Status != "sent" {
		t.Fatalf("dispatch status = %q, want sent", payload.Dispatch.Status)
	}
	if payload.Dispatch.FreshContext {
		t.Fatal("expected default start_task to avoid fresh-context restart")
	}
}

func TestStartTaskHandlerFreshModeRestartsBeforeWake(t *testing.T) {
	var steps []string
	restorePrepare := stubPrepareFreshWorkspaceForTask(func(client *DaemonClient, configPath, target string) error {
		steps = append(steps, "restart:"+target)
		if configPath != "/tmp/ax-config.yaml" {
			return fmt.Errorf("unexpected config path %q", configPath)
		}
		return nil
	})
	defer restorePrepare()
	restoreWake := stubWakeWorkspaceAgent(func(target, sender string, fresh bool) {
		steps = append(steps, fmt.Sprintf("wake:%s:%t", target, fresh))
	})
	defer restoreWake()

	const taskID = "22222222-2222-2222-2222-222222222222"

	client, serverErr := newTaskToolTestClient(t, 2, func(step int, env *daemon.Envelope) (*daemon.Envelope, error) {
		switch step {
		case 0:
			var payload daemon.CreateTaskPayload
			if err := env.DecodePayload(&payload); err != nil {
				return nil, err
			}
			if payload.StartMode != string(types.TaskStartFresh) {
				return nil, fmt.Errorf("unexpected start mode %q", payload.StartMode)
			}
			return daemon.NewResponseEnvelope(env.ID, &daemon.TaskResponse{
				Task: types.Task{
					ID:        taskID,
					Title:     payload.Title,
					Assignee:  payload.Assignee,
					CreatedBy: "tester",
					Status:    types.TaskPending,
					StartMode: types.TaskStartFresh,
				},
			})
		case 1:
			var payload daemon.SendMessagePayload
			if err := env.DecodePayload(&payload); err != nil {
				return nil, err
			}
			if !strings.Contains(payload.Message, "Task ID: "+taskID) {
				return nil, fmt.Errorf("dispatch message missing task id: %q", payload.Message)
			}
			return daemon.NewResponseEnvelope(env.ID, map[string]string{
				"message_id": "msg-fresh",
				"status":     "sent",
			})
		default:
			return nil, fmt.Errorf("unexpected request step %d", step)
		}
	})

	result, err := startTaskHandler(client, "/tmp/ax-config.yaml")(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"title":      "Fresh restart task",
				"assignee":   "worker",
				"message":    "Restart from a clean session",
				"start_mode": "fresh",
			},
		},
	})
	if err != nil {
		t.Fatalf("startTaskHandler returned error: %v", err)
	}
	if err := <-serverErr; err != nil {
		t.Fatalf("daemon stub failed: %v", err)
	}

	if got, want := strings.Join(steps, ","), "restart:worker,wake:worker:true"; got != want {
		t.Fatalf("step order = %q, want %q", got, want)
	}

	var payload startTaskResult
	decodeToolResultJSON(t, result, &payload)
	if !payload.Dispatch.FreshContext {
		t.Fatal("expected fresh start_task dispatch to report fresh_context=true")
	}
}

func TestNormalizeStartTaskMessageRejectsEmbeddedTaskID(t *testing.T) {
	_, err := normalizeStartTaskMessage("Task ID: 33333333-3333-3333-3333-333333333333\n\nPlease handle this")
	if err == nil {
		t.Fatal("expected embedded Task ID to be rejected")
	}
	if !strings.Contains(err.Error(), "start_task injects the new task ID automatically") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func newTaskToolTestClient(t *testing.T, expectedRequests int, handler func(step int, env *daemon.Envelope) (*daemon.Envelope, error)) (*DaemonClient, <-chan error) {
	t.Helper()

	clientConn, serverConn := net.Pipe()
	client := NewDaemonClient("", "tester")
	client.conn = clientConn
	client.connected.Store(true)
	client.setDisconnectErr(nil)

	go client.readLoop()

	serverErr := make(chan error, 1)
	go func() {
		defer close(serverErr)
		defer serverConn.Close()

		scanner := bufio.NewScanner(serverConn)
		scanner.Buffer(make([]byte, 1024*1024), 1024*1024)
		for step := 0; step < expectedRequests; step++ {
			if !scanner.Scan() {
				serverErr <- fmt.Errorf("expected request %d, scanner err=%v", step+1, scanner.Err())
				return
			}

			var env daemon.Envelope
			if err := json.Unmarshal(scanner.Bytes(), &env); err != nil {
				serverErr <- fmt.Errorf("decode request: %w", err)
				return
			}

			resp, err := handler(step, &env)
			if err != nil {
				serverErr <- err
				return
			}
			if resp == nil {
				serverErr <- fmt.Errorf("handler returned nil response for step %d", step)
				return
			}

			data, err := json.Marshal(resp)
			if err != nil {
				serverErr <- fmt.Errorf("marshal response: %w", err)
				return
			}
			if _, err := serverConn.Write(append(data, '\n')); err != nil {
				serverErr <- fmt.Errorf("write response: %w", err)
				return
			}
		}

		serverErr <- nil
	}()

	t.Cleanup(func() {
		_ = client.Close()
	})

	return client, serverErr
}

func stubWakeWorkspaceAgent(fn func(target, sender string, fresh bool)) func() {
	original := wakeWorkspaceAgent
	wakeWorkspaceAgent = fn
	return func() {
		wakeWorkspaceAgent = original
	}
}

func stubPrepareFreshWorkspaceForTask(fn func(client *DaemonClient, configPath, target string) error) func() {
	original := prepareFreshWorkspaceForTask
	prepareFreshWorkspaceForTask = fn
	return func() {
		prepareFreshWorkspaceForTask = original
	}
}
