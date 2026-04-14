package mcpserver

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/types"
	"github.com/mark3labs/mcp-go/mcp"
)

func TestListAgentsIncludesDerivedStateAndStatusText(t *testing.T) {
	cfgPath := writeToolConfig(t, `
project: demo
workspaces:
  idle:
    dir: .
    description: idle agent
    runtime: claude
    instructions: owns triage and inbox handling
  offline:
    dir: ./offline
    description: offline manual agent
    agent: none
  running:
    dir: ./running
    description: running custom agent
    agent: custom-runner
`)

	now := time.Now().UTC()
	client, serverErr := newListAgentsTestClient(t, []types.WorkspaceInfo{
		{
			Name:        "idle",
			Dir:         "/tmp/idle",
			Description: "idle agent",
			Status:      types.StatusOnline,
			StatusText:  "Waiting for task",
			ConnectedAt: &now,
		},
		{
			Name:        "running",
			Dir:         "/tmp/running",
			Description: "running custom agent",
			Status:      types.StatusOnline,
			StatusText:  "Applying patch",
			ConnectedAt: &now,
		},
	})

	restoreIdle := stubWorkspaceIsIdle(func(workspace string) bool {
		return workspace == "idle"
	})
	defer restoreIdle()

	result, err := listAgentsHandler(client, cfgPath)(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{},
		},
	})
	if err != nil {
		t.Fatalf("listAgentsHandler returned error: %v", err)
	}
	if err := <-serverErr; err != nil {
		t.Fatalf("daemon stub failed: %v", err)
	}

	var payload struct {
		Project     string      `json:"project"`
		AgentCount  int         `json:"agent_count"`
		ActiveCount int         `json:"active_count"`
		Agents      []agentInfo `json:"agents"`
	}
	decodeToolResultJSON(t, result, &payload)

	if payload.Project != "demo" {
		t.Fatalf("expected project demo, got %q", payload.Project)
	}
	if payload.AgentCount != 3 {
		t.Fatalf("expected 3 agents, got %d", payload.AgentCount)
	}
	if payload.ActiveCount != 2 {
		t.Fatalf("expected 2 active agents, got %d", payload.ActiveCount)
	}

	agentsByName := make(map[string]agentInfo, len(payload.Agents))
	for _, info := range payload.Agents {
		agentsByName[info.Name] = info
	}

	idle := agentsByName["idle"]
	if !idle.Active {
		t.Fatalf("expected idle agent to remain active")
	}
	if idle.State != agentStateIdle {
		t.Fatalf("expected idle state %q, got %q", agentStateIdle, idle.State)
	}
	if idle.StatusText != "Waiting for task" {
		t.Fatalf("expected idle status_text to be preserved, got %q", idle.StatusText)
	}
	if idle.Status != types.StatusOnline {
		t.Fatalf("expected idle status %q, got %q", types.StatusOnline, idle.Status)
	}
	if idle.Runtime != "claude" || idle.LaunchMode != "runtime" {
		t.Fatalf("expected runtime launch metadata to remain intact, got runtime=%q launch_mode=%q", idle.Runtime, idle.LaunchMode)
	}
	if idle.Instruction == "" {
		t.Fatal("expected instruction file for runtime agent")
	}

	running := agentsByName["running"]
	if !running.Active {
		t.Fatalf("expected running agent to remain active")
	}
	if running.State != agentStateRunning {
		t.Fatalf("expected running state %q, got %q", agentStateRunning, running.State)
	}
	if running.StatusText != "Applying patch" {
		t.Fatalf("expected running status_text to be preserved, got %q", running.StatusText)
	}
	if running.Command != "custom-runner" || running.LaunchMode != "custom" {
		t.Fatalf("expected custom launch metadata to remain intact, got command=%q launch_mode=%q", running.Command, running.LaunchMode)
	}

	offline := agentsByName["offline"]
	if offline.Active {
		t.Fatalf("expected offline agent to be inactive")
	}
	if offline.State != agentStateOffline {
		t.Fatalf("expected offline state %q, got %q", agentStateOffline, offline.State)
	}
	if offline.LaunchMode != "manual" {
		t.Fatalf("expected manual launch mode to remain intact, got %q", offline.LaunchMode)
	}
	if offline.StatusText != "" {
		t.Fatalf("expected offline agent to omit status_text, got %q", offline.StatusText)
	}
}

func TestListAgentsQueryMatchesDerivedState(t *testing.T) {
	cfgPath := writeToolConfig(t, `
project: demo
workspaces:
  builder:
    dir: .
    description: handles implementation
  reviewer:
    dir: ./reviewer
    description: handles review
`)

	now := time.Now().UTC()
	client, serverErr := newListAgentsTestClient(t, []types.WorkspaceInfo{
		{
			Name:        "builder",
			Dir:         "/tmp/builder",
			Description: "handles implementation",
			Status:      types.StatusOnline,
			ConnectedAt: &now,
		},
		{
			Name:        "reviewer",
			Dir:         "/tmp/reviewer",
			Description: "handles review",
			Status:      types.StatusOnline,
			ConnectedAt: &now,
		},
	})

	restoreIdle := stubWorkspaceIsIdle(func(workspace string) bool {
		return workspace == "reviewer"
	})
	defer restoreIdle()

	result, err := listAgentsHandler(client, cfgPath)(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"query": "running",
			},
		},
	})
	if err != nil {
		t.Fatalf("listAgentsHandler returned error: %v", err)
	}
	if err := <-serverErr; err != nil {
		t.Fatalf("daemon stub failed: %v", err)
	}

	var payload struct {
		Agents []agentInfo `json:"agents"`
	}
	decodeToolResultJSON(t, result, &payload)

	if len(payload.Agents) != 1 {
		t.Fatalf("expected one agent matched by derived state, got %+v", payload.Agents)
	}
	if payload.Agents[0].Name != "builder" {
		t.Fatalf("expected builder to match running query, got %q", payload.Agents[0].Name)
	}
	if payload.Agents[0].State != agentStateRunning {
		t.Fatalf("expected builder state %q, got %q", agentStateRunning, payload.Agents[0].State)
	}
}

func newListAgentsTestClient(t *testing.T, workspaces []types.WorkspaceInfo) (*DaemonClient, <-chan error) {
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
		if !scanner.Scan() {
			serverErr <- fmt.Errorf("expected list_workspaces request, scanner err=%v", scanner.Err())
			return
		}

		var env daemon.Envelope
		if err := json.Unmarshal(scanner.Bytes(), &env); err != nil {
			serverErr <- fmt.Errorf("decode request: %w", err)
			return
		}
		if env.Type != daemon.MsgListWorkspaces {
			serverErr <- fmt.Errorf("unexpected request type %s", env.Type)
			return
		}

		resp, err := daemon.NewResponseEnvelope(env.ID, &daemon.ListWorkspacesResponse{Workspaces: workspaces})
		if err != nil {
			serverErr <- fmt.Errorf("build response: %w", err)
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

		serverErr <- nil
	}()

	t.Cleanup(func() {
		_ = client.Close()
	})

	return client, serverErr
}

func stubWorkspaceIsIdle(fn func(workspace string) bool) func() {
	original := workspaceIsIdle
	workspaceIsIdle = fn
	return func() {
		workspaceIsIdle = original
	}
}

func writeToolConfig(t *testing.T, body string) string {
	t.Helper()

	root := t.TempDir()
	configPath := filepath.Join(root, ".ax", "config.yaml")
	if err := os.MkdirAll(filepath.Dir(configPath), 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(configPath, []byte(body), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	return configPath
}

func decodeToolResultJSON(t *testing.T, result *mcp.CallToolResult, target any) {
	t.Helper()

	if result == nil {
		t.Fatal("expected tool result, got nil")
	}
	if result.IsError {
		t.Fatalf("expected successful tool result, got error: %+v", result.Content)
	}
	if len(result.Content) != 1 {
		t.Fatalf("expected one content item, got %d", len(result.Content))
	}

	text, ok := result.Content[0].(mcp.TextContent)
	if !ok {
		t.Fatalf("expected text content, got %T", result.Content[0])
	}
	if err := json.Unmarshal([]byte(text.Text), target); err != nil {
		t.Fatalf("decode tool result JSON: %v\npayload: %s", err, text.Text)
	}
}
