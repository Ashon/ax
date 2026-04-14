package mcpserver

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/types"
	"github.com/mark3labs/mcp-go/mcp"
)

func TestApplyTeamReconfigureHandlerAddsAndRemovesWorkspaceThroughMCP(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	projectDir := filepath.Join(home, "project")
	cfgPath := writeMCPTeamConfig(t, projectDir, `
project: demo
experimental_mcp_team_reconfigure: true
workspaces:
  main:
    dir: .
    runtime: claude
`)

	socketPath, cancel := startMCPTestDaemon(t)
	defer cancel()

	client := NewDaemonClient(socketPath, "tester")
	if err := client.Connect(); err != nil {
		t.Fatalf("connect client: %v", err)
	}
	defer client.Close()

	if err := client.SetSharedValue(types.ExperimentalMCPTeamReconfigureFlagKey, "true"); err != nil {
		t.Fatalf("set feature flag: %v", err)
	}

	applyResult, err := applyTeamReconfigureHandler(client, cfgPath)(context.Background(), toolRequest("apply_team_reconfigure", map[string]any{
		"changes": []map[string]any{
			{
				"op":   string(types.TeamChangeAdd),
				"kind": string(types.TeamEntryWorkspace),
				"name": "helper",
				"workspace": map[string]any{
					"dir":         "./helper",
					"description": "helper workspace",
					"runtime":     "claude",
				},
			},
		},
		"reconcile_mode": string(types.TeamReconcileArtifactsOnly),
	}))
	if err != nil {
		t.Fatalf("apply tool returned protocol error: %v", err)
	}
	if applyResult.IsError {
		t.Fatalf("expected successful tool result, got error content: %s", toolResultText(t, applyResult))
	}

	var applyPayload teamApplyResult
	if err := json.Unmarshal([]byte(toolResultText(t, applyResult)), &applyPayload); err != nil {
		t.Fatalf("decode apply result: %v", err)
	}
	if applyPayload.State.Revision != 1 {
		t.Fatalf("expected revision 1 after apply, got %d", applyPayload.State.Revision)
	}
	if applyPayload.State.LastApply == nil || !applyPayload.State.LastApply.Success {
		t.Fatalf("expected successful apply report, got %+v", applyPayload.State.LastApply)
	}
	if applyPayload.State.EffectiveConfigPath == cfgPath {
		t.Fatalf("expected managed effective config path, got base path %q", applyPayload.State.EffectiveConfigPath)
	}
	if _, err := os.Stat(filepath.Join(projectDir, "helper", ".mcp.json")); err != nil {
		t.Fatalf("expected helper workspace artifacts to be reconciled: %v", err)
	}

	listResult, err := listAgentsHandler(client, cfgPath)(context.Background(), toolRequest("list_agents", nil))
	if err != nil {
		t.Fatalf("list_agents returned protocol error: %v", err)
	}
	if listResult.IsError {
		t.Fatalf("expected successful list_agents result, got %s", toolResultText(t, listResult))
	}

	var listed struct {
		ConfigPath string      `json:"config_path"`
		Agents     []agentInfo `json:"agents"`
	}
	if err := json.Unmarshal([]byte(toolResultText(t, listResult)), &listed); err != nil {
		t.Fatalf("decode list_agents result: %v", err)
	}
	if listed.ConfigPath != applyPayload.State.EffectiveConfigPath {
		t.Fatalf("expected list_agents to load effective config %q, got %q", applyPayload.State.EffectiveConfigPath, listed.ConfigPath)
	}
	if !containsAgent(listed.Agents, "helper") {
		t.Fatalf("expected helper workspace in effective agent list, got %+v", listed.Agents)
	}

	removeResult, err := applyTeamReconfigureHandler(client, cfgPath)(context.Background(), toolRequest("apply_team_reconfigure", map[string]any{
		"expected_revision": float64(applyPayload.State.Revision),
		"changes": []map[string]any{
			{
				"op":   string(types.TeamChangeRemove),
				"kind": string(types.TeamEntryWorkspace),
				"name": "helper",
			},
		},
		"reconcile_mode": string(types.TeamReconcileArtifactsOnly),
	}))
	if err != nil {
		t.Fatalf("remove apply returned protocol error: %v", err)
	}
	if removeResult.IsError {
		t.Fatalf("expected successful remove result, got error content: %s", toolResultText(t, removeResult))
	}

	var removePayload teamApplyResult
	if err := json.Unmarshal([]byte(toolResultText(t, removeResult)), &removePayload); err != nil {
		t.Fatalf("decode remove apply result: %v", err)
	}
	if removePayload.State.Revision != 2 {
		t.Fatalf("expected revision 2 after remove apply, got %d", removePayload.State.Revision)
	}
	if removePayload.State.LastApply == nil || !removePayload.State.LastApply.Success {
		t.Fatalf("expected successful remove apply report, got %+v", removePayload.State.LastApply)
	}
	if containsAgentName(removePayload.State.Desired.Workspaces, "helper") {
		t.Fatalf("expected helper to be absent from desired workspaces after remove, got %+v", removePayload.State.Desired.Workspaces)
	}
	if _, err := os.Stat(filepath.Join(projectDir, "helper", ".mcp.json")); !os.IsNotExist(err) {
		t.Fatalf("expected helper artifacts to be removed after MCP remove, stat err=%v", err)
	}

	listResult, err = listAgentsHandler(client, cfgPath)(context.Background(), toolRequest("list_agents", nil))
	if err != nil {
		t.Fatalf("list_agents after remove returned protocol error: %v", err)
	}
	if listResult.IsError {
		t.Fatalf("expected successful list_agents result after remove, got %s", toolResultText(t, listResult))
	}

	if err := json.Unmarshal([]byte(toolResultText(t, listResult)), &listed); err != nil {
		t.Fatalf("decode list_agents after remove result: %v", err)
	}
	if listed.ConfigPath != removePayload.State.EffectiveConfigPath {
		t.Fatalf("expected list_agents after remove to load effective config %q, got %q", removePayload.State.EffectiveConfigPath, listed.ConfigPath)
	}
	if containsAgent(listed.Agents, "helper") {
		t.Fatalf("expected helper workspace to be absent after remove, got %+v", listed.Agents)
	}
}

func startMCPTestDaemon(t *testing.T) (string, context.CancelFunc) {
	t.Helper()
	stateDir, err := os.MkdirTemp("", "axd-")
	if err != nil {
		t.Fatalf("mkdtemp: %v", err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(stateDir) })
	socketPath := filepath.Join(stateDir, "daemon.sock")
	ctx, cancel := context.WithCancel(context.Background())
	d := daemon.New(socketPath)
	errCh := make(chan error, 1)
	go func() {
		errCh <- d.Run(ctx)
	}()
	for i := 0; i < 100; i++ {
		if _, err := os.Stat(socketPath); err == nil {
			return socketPath, cancel
		}
		select {
		case err := <-errCh:
			t.Fatalf("daemon exited before socket was ready: %v", err)
		default:
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("daemon socket %s did not appear", socketPath)
	return "", cancel
}

func writeMCPTeamConfig(t *testing.T, rootDir, content string) string {
	t.Helper()
	cfgPath := filepath.Join(rootDir, ".ax", "config.yaml")
	if err := os.MkdirAll(filepath.Dir(cfgPath), 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(cfgPath, []byte(content), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	return cfgPath
}

func toolRequest(name string, args map[string]any) mcp.CallToolRequest {
	return mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Name:      name,
			Arguments: args,
		},
	}
}

func toolResultText(t *testing.T, result *mcp.CallToolResult) string {
	t.Helper()
	if result == nil || len(result.Content) == 0 {
		t.Fatal("expected text tool result content")
	}
	text, ok := mcp.AsTextContent(result.Content[0])
	if !ok {
		t.Fatalf("expected text content, got %T", result.Content[0])
	}
	return text.Text
}

func containsAgent(agents []agentInfo, name string) bool {
	for _, agent := range agents {
		if agent.Name == name {
			return true
		}
	}
	return false
}

func containsAgentName(names []string, target string) bool {
	for _, name := range names {
		if name == target {
			return true
		}
	}
	return false
}
