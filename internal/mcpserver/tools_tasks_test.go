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
	const taskID = "11111111-1111-1111-1111-111111111111"

	client, serverErr := newTaskToolTestClient(t, 1, func(step int, env *daemon.Envelope) (*daemon.Envelope, error) {
		switch step {
		case 0:
			if env.Type != daemon.MsgStartTask {
				return nil, fmt.Errorf("step 0 request type = %s, want %s", env.Type, daemon.MsgStartTask)
			}
			var payload daemon.StartTaskPayload
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
			if payload.Message != "Inspect the fresh start handoff" {
				return nil, fmt.Errorf("unexpected message %q", payload.Message)
			}
			if payload.StartMode != string(types.TaskStartDefault) {
				return nil, fmt.Errorf("unexpected start mode %q", payload.StartMode)
			}
			if payload.WorkflowMode != string(types.TaskWorkflowParallel) {
				return nil, fmt.Errorf("unexpected workflow mode %q", payload.WorkflowMode)
			}
			if payload.Priority != string(types.TaskPriorityHigh) {
				return nil, fmt.Errorf("unexpected priority %q", payload.Priority)
			}
			if payload.StaleAfterSeconds != 300 {
				return nil, fmt.Errorf("unexpected stale_after_seconds %d", payload.StaleAfterSeconds)
			}
			return daemon.NewResponseEnvelope(env.ID, &daemon.StartTaskResponse{
				Task: types.Task{
					ID:           taskID,
					Title:        payload.Title,
					Description:  payload.Description,
					Assignee:     payload.Assignee,
					CreatedBy:    "tester",
					Status:       types.TaskPending,
					StartMode:    types.TaskStartDefault,
					WorkflowMode: types.TaskWorkflowParallel,
					Priority:     types.TaskPriorityHigh,
				},
				Dispatch: daemon.TaskDispatch{
					MessageID: "msg-1",
					Status:    "queued",
				},
			})
		default:
			return nil, fmt.Errorf("unexpected request step %d", step)
		}
	})

	result, err := startTaskHandler(client)(context.Background(), mcp.CallToolRequest{
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
	if payload.Dispatch.Status != "queued" {
		t.Fatalf("dispatch status = %q, want queued", payload.Dispatch.Status)
	}
	if payload.Task.WorkflowMode != types.TaskWorkflowParallel {
		t.Fatalf("workflow_mode = %q, want %q", payload.Task.WorkflowMode, types.TaskWorkflowParallel)
	}
}

func TestStartTaskHandlerReturnsWaitingTurnForSerialChild(t *testing.T) {
	const taskID = "22222222-2222-2222-2222-222222222222"
	const parentTaskID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
	const waitingOnTaskID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"

	client, serverErr := newTaskToolTestClient(t, 1, func(step int, env *daemon.Envelope) (*daemon.Envelope, error) {
		switch step {
		case 0:
			if env.Type != daemon.MsgStartTask {
				return nil, fmt.Errorf("step 0 request type = %s, want %s", env.Type, daemon.MsgStartTask)
			}
			var payload daemon.StartTaskPayload
			if err := env.DecodePayload(&payload); err != nil {
				return nil, err
			}
			if payload.StartMode != string(types.TaskStartFresh) {
				return nil, fmt.Errorf("unexpected start mode %q", payload.StartMode)
			}
			if payload.ParentTaskID != parentTaskID {
				return nil, fmt.Errorf("unexpected parent task id %q", payload.ParentTaskID)
			}
			if payload.WorkflowMode != string(types.TaskWorkflowParallel) {
				return nil, fmt.Errorf("unexpected child workflow mode %q", payload.WorkflowMode)
			}
			return daemon.NewResponseEnvelope(env.ID, &daemon.StartTaskResponse{
				Task: types.Task{
					ID:           taskID,
					Title:        payload.Title,
					Assignee:     payload.Assignee,
					CreatedBy:    "tester",
					ParentTaskID: payload.ParentTaskID,
					Status:       types.TaskPending,
					StartMode:    types.TaskStartFresh,
					WorkflowMode: types.TaskWorkflowParallel,
					Sequence: &types.TaskSequenceInfo{
						Mode:            types.TaskWorkflowSerial,
						State:           types.TaskSequenceWaitingTurn,
						Position:        2,
						WaitingOnTaskID: waitingOnTaskID,
					},
				},
				Dispatch: daemon.TaskDispatch{
					Status: "waiting_turn",
				},
			})
		default:
			return nil, fmt.Errorf("unexpected request step %d", step)
		}
	})

	result, err := startTaskHandler(client)(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"title":          "Serial child",
				"assignee":       "worker",
				"message":        "Wait until the first child is terminal",
				"parent_task_id": parentTaskID,
				"start_mode":     "fresh",
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
	if payload.Dispatch.Status != "waiting_turn" {
		t.Fatalf("dispatch status = %q, want waiting_turn", payload.Dispatch.Status)
	}
	if payload.Dispatch.MessageID != "" {
		t.Fatalf("dispatch message_id = %q, want empty", payload.Dispatch.MessageID)
	}
	if payload.Task.Sequence == nil {
		t.Fatal("expected sequence info for serial child")
	}
	if payload.Task.Sequence.State != types.TaskSequenceWaitingTurn {
		t.Fatalf("sequence.state = %q, want %q", payload.Task.Sequence.State, types.TaskSequenceWaitingTurn)
	}
	if payload.Task.Sequence.WaitingOnTaskID != waitingOnTaskID {
		t.Fatalf("waiting_on_task_id = %q, want %q", payload.Task.Sequence.WaitingOnTaskID, waitingOnTaskID)
	}
}

func TestCreateTaskHandlerPassesWorkflowMode(t *testing.T) {
	const taskID = "44444444-4444-4444-4444-444444444444"

	client, serverErr := newTaskToolTestClient(t, 1, func(step int, env *daemon.Envelope) (*daemon.Envelope, error) {
		if step != 0 {
			return nil, fmt.Errorf("unexpected request step %d", step)
		}
		if env.Type != daemon.MsgCreateTask {
			return nil, fmt.Errorf("request type = %s, want %s", env.Type, daemon.MsgCreateTask)
		}
		var payload daemon.CreateTaskPayload
		if err := env.DecodePayload(&payload); err != nil {
			return nil, err
		}
		if payload.WorkflowMode != string(types.TaskWorkflowSerial) {
			return nil, fmt.Errorf("unexpected workflow mode %q", payload.WorkflowMode)
		}
		return daemon.NewResponseEnvelope(env.ID, &daemon.TaskResponse{
			Task: types.Task{
				ID:           taskID,
				Title:        payload.Title,
				Assignee:     payload.Assignee,
				CreatedBy:    "tester",
				Status:       types.TaskPending,
				WorkflowMode: types.TaskWorkflowSerial,
			},
		})
	})

	result, err := createTaskHandler(client)(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"title":         "Serial parent",
				"assignee":      "worker",
				"workflow_mode": "serial",
			},
		},
	})
	if err != nil {
		t.Fatalf("createTaskHandler returned error: %v", err)
	}
	if err := <-serverErr; err != nil {
		t.Fatalf("daemon stub failed: %v", err)
	}

	var payload types.Task
	decodeToolResultJSON(t, result, &payload)
	if payload.WorkflowMode != types.TaskWorkflowSerial {
		t.Fatalf("workflow_mode = %q, want %q", payload.WorkflowMode, types.TaskWorkflowSerial)
	}
}

func TestListWorkspaceTasksHandlerReturnsExplicitViews(t *testing.T) {
	const sharedTaskID = "55555555-5555-5555-5555-555555555555"
	const createdOnlyTaskID = "66666666-6666-6666-6666-666666666666"

	client, serverErr := newTaskToolTestClient(t, 2, func(step int, env *daemon.Envelope) (*daemon.Envelope, error) {
		if env.Type != daemon.MsgListTasks {
			return nil, fmt.Errorf("step %d request type = %s, want %s", step, env.Type, daemon.MsgListTasks)
		}

		var payload daemon.ListTasksPayload
		if err := env.DecodePayload(&payload); err != nil {
			return nil, err
		}
		if payload.Status == nil || *payload.Status != types.TaskInProgress {
			return nil, fmt.Errorf("unexpected status filter %+v", payload.Status)
		}

		switch step {
		case 0:
			if payload.Assignee != "builder" || payload.CreatedBy != "" {
				return nil, fmt.Errorf("unexpected assigned-view payload assignee=%q created_by=%q", payload.Assignee, payload.CreatedBy)
			}
			return daemon.NewResponseEnvelope(env.ID, &daemon.ListTasksResponse{
				Tasks: []types.Task{
					{
						ID:        sharedTaskID,
						Title:     "Assigned to builder",
						Assignee:  "builder",
						CreatedBy: "reviewer",
						Status:    types.TaskInProgress,
					},
				},
			})
		case 1:
			if payload.Assignee != "" || payload.CreatedBy != "builder" {
				return nil, fmt.Errorf("unexpected created-view payload assignee=%q created_by=%q", payload.Assignee, payload.CreatedBy)
			}
			return daemon.NewResponseEnvelope(env.ID, &daemon.ListTasksResponse{
				Tasks: []types.Task{
					{
						ID:        sharedTaskID,
						Title:     "Assigned to builder",
						Assignee:  "builder",
						CreatedBy: "reviewer",
						Status:    types.TaskInProgress,
					},
					{
						ID:        createdOnlyTaskID,
						Title:     "Created by builder",
						Assignee:  "runner",
						CreatedBy: "builder",
						Status:    types.TaskInProgress,
					},
				},
			})
		default:
			return nil, fmt.Errorf("unexpected request step %d", step)
		}
	})

	result, err := listWorkspaceTasksHandler(client)(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"workspace": "builder",
				"view":      "both",
				"status":    "in_progress",
			},
		},
	})
	if err != nil {
		t.Fatalf("listWorkspaceTasksHandler returned error: %v", err)
	}
	if err := <-serverErr; err != nil {
		t.Fatalf("daemon stub failed: %v", err)
	}

	var payload workspaceTaskListResult
	decodeToolResultJSON(t, result, &payload)
	if payload.Workspace != "builder" {
		t.Fatalf("workspace = %q, want builder", payload.Workspace)
	}
	if payload.View != "both" {
		t.Fatalf("view = %q, want both", payload.View)
	}
	if payload.Status != "in_progress" {
		t.Fatalf("status = %q, want in_progress", payload.Status)
	}
	if payload.Assigned == nil || payload.Assigned.Count != 1 {
		t.Fatalf("assigned view = %+v, want count=1", payload.Assigned)
	}
	if payload.Created == nil || payload.Created.Count != 2 {
		t.Fatalf("created view = %+v, want count=2", payload.Created)
	}
	if payload.UniqueTaskCount != 2 {
		t.Fatalf("unique_task_count = %d, want 2", payload.UniqueTaskCount)
	}
}

func TestParseTaskCreateOptionsValidation(t *testing.T) {
	t.Run("rejects negative stale threshold", func(t *testing.T) {
		_, _, _, _, _, _, err := parseTaskCreateOptions(mcp.CallToolRequest{
			Params: mcp.CallToolParams{
				Arguments: map[string]any{
					"stale_after_seconds": -1,
				},
			},
		})
		if err == nil {
			t.Fatal("expected negative stale_after_seconds to fail")
		}
		if !strings.Contains(err.Error(), "must be >= 0") {
			t.Fatalf("unexpected error: %v", err)
		}
	})

	t.Run("rejects unknown start_mode", func(t *testing.T) {
		_, _, _, _, _, _, err := parseTaskCreateOptions(mcp.CallToolRequest{
			Params: mcp.CallToolParams{
				Arguments: map[string]any{
					"start_mode": "later",
				},
			},
		})
		if err == nil {
			t.Fatal("expected invalid start_mode to fail")
		}
		if !strings.Contains(err.Error(), "must be default or fresh") {
			t.Fatalf("unexpected error: %v", err)
		}
	})

	t.Run("rejects unknown workflow_mode", func(t *testing.T) {
		_, _, _, _, _, _, err := parseTaskCreateOptions(mcp.CallToolRequest{
			Params: mcp.CallToolParams{
				Arguments: map[string]any{
					"workflow_mode": "fanout",
				},
			},
		})
		if err == nil {
			t.Fatal("expected invalid workflow_mode to fail")
		}
		if !strings.Contains(err.Error(), "must be parallel or serial") {
			t.Fatalf("unexpected error: %v", err)
		}
	})

	t.Run("rejects unknown priority", func(t *testing.T) {
		_, _, _, _, _, _, err := parseTaskCreateOptions(mcp.CallToolRequest{
			Params: mcp.CallToolParams{
				Arguments: map[string]any{
					"priority": "p0",
				},
			},
		})
		if err == nil {
			t.Fatal("expected invalid priority to fail")
		}
		if !strings.Contains(err.Error(), "must be low, normal, high, or urgent") {
			t.Fatalf("unexpected error: %v", err)
		}
	})

	t.Run("rejects unknown workspace task view", func(t *testing.T) {
		_, err := parseWorkspaceTaskView("owner")
		if err == nil {
			t.Fatal("expected invalid workspace task view to fail")
		}
		if !strings.Contains(err.Error(), "must be assigned, created, or both") {
			t.Fatalf("unexpected error: %v", err)
		}
	})

	t.Run("rejects unknown task list status filter", func(t *testing.T) {
		_, err := parseListTaskStatusFilter("waiting")
		if err == nil {
			t.Fatal("expected invalid status filter to fail")
		}
		if !strings.Contains(err.Error(), "Invalid status filter") {
			t.Fatalf("unexpected error: %v", err)
		}
	})
}

func newTaskToolTestClient(t *testing.T, expectedRequests int, handler func(step int, env *daemon.Envelope) (*daemon.Envelope, error)) (*DaemonClient, <-chan error) {
	t.Helper()

	clientConn, serverConn := net.Pipe()
	client := NewDaemonClient("/tmp/ax.sock", "tester")
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
