package mcpserver

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/workspace"
	"github.com/mark3labs/mcp-go/mcp"
)

type stubLifecycleWorkspaceManager struct {
	createFn  func(name string, ws config.Workspace) error
	restartFn func(name string, ws config.Workspace) error
	destroyFn func(name, dir string) error
}

func (m stubLifecycleWorkspaceManager) Create(name string, ws config.Workspace) error {
	if m.createFn == nil {
		return nil
	}
	return m.createFn(name, ws)
}

func (m stubLifecycleWorkspaceManager) Restart(name string, ws config.Workspace) error {
	if m.restartFn == nil {
		return nil
	}
	return m.restartFn(name, ws)
}

func (m stubLifecycleWorkspaceManager) Destroy(name, dir string) error {
	if m.destroyFn == nil {
		return nil
	}
	return m.destroyFn(name, dir)
}

func TestStartAgentHandlerStartsWorkspaceByExactName(t *testing.T) {
	cfgPath := writeToolConfig(t, `
project: demo
workspaces:
  worker:
    dir: ./worker
    runtime: claude
`)

	stubAgentLifecycleOps(t)
	sessionExists := false
	newLifecycleWorkspaceManager = func(socketPath, configPath string) lifecycleWorkspaceManager {
		return stubLifecycleWorkspaceManager{
			createFn: func(name string, ws config.Workspace) error {
				if name != "worker" {
					return fmt.Errorf("unexpected workspace %q", name)
				}
				if ws.Dir != filepath.Join(filepath.Dir(filepath.Dir(cfgPath)), "worker") {
					return fmt.Errorf("unexpected workspace dir %q", ws.Dir)
				}
				sessionExists = true
				return nil
			},
		}
	}
	lifecycleSessionExists = func(name string) bool {
		return name == "worker" && sessionExists
	}

	client := NewDaemonClient("/tmp/ax.sock", "tester")
	result, err := agentLifecycleHandler(client, cfgPath, agentLifecycleActionStart)(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"name": "worker",
			},
		},
	})
	if err != nil {
		t.Fatalf("start handler returned error: %v", err)
	}

	var payload agentLifecycleResult
	decodeToolResultJSON(t, result, &payload)

	if payload.Status != "started" {
		t.Fatalf("expected status started, got %q", payload.Status)
	}
	if payload.TargetKind != "workspace" {
		t.Fatalf("expected workspace target kind, got %q", payload.TargetKind)
	}
	if payload.SessionExistsBefore {
		t.Fatal("expected workspace to start from a stopped state")
	}
	if !payload.SessionExistsAfter {
		t.Fatal("expected workspace session after start")
	}
	if !payload.ExactMatch {
		t.Fatal("expected exact_match to be true")
	}
}

func TestStartAgentHandlerRejectsUnknownName(t *testing.T) {
	cfgPath := writeToolConfig(t, `
project: demo
workspaces:
  worker:
    dir: ./worker
    runtime: claude
`)

	client := NewDaemonClient("/tmp/ax.sock", "tester")
	result, err := agentLifecycleHandler(client, cfgPath, agentLifecycleActionStart)(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"name": "work",
			},
		},
	})
	if err != nil {
		t.Fatalf("start handler returned error: %v", err)
	}
	assertToolErrorContains(t, result, `Agent "work" is not defined exactly`)
}

func TestRestartAgentHandlerRestartsManagedChildOrchestrator(t *testing.T) {
	root := t.TempDir()
	childDir := filepath.Join(root, "child")
	rootConfigPath := filepath.Join(root, ".ax", "config.yaml")
	writeLifecycleConfig(t, rootConfigPath, `
project: root
workspaces:
  root:
    dir: .
    runtime: claude
children:
  child:
    dir: ./child
    prefix: team
`)
	writeLifecycleConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
project: child
orchestrator_runtime: claude
workspaces:
  dev:
    dir: .
    runtime: claude
`)

	stubAgentLifecycleOps(t)
	sessionExists := true
	var steps []string
	lifecycleSessionExists = func(name string) bool {
		return name == "team.orchestrator" && sessionExists
	}
	cleanupLifecycleOrchestratorState = func(name, dir string) error {
		steps = append(steps, "cleanup:"+name)
		if name != "team.orchestrator" {
			return fmt.Errorf("unexpected cleanup target %q", name)
		}
		if dir != filepath.Join(childDir, ".ax", "orchestrator-team") {
			return fmt.Errorf("unexpected artifact dir %q", dir)
		}
		sessionExists = false
		return nil
	}
	ensureLifecycleOrchestrator = func(node *config.ProjectNode, parentName, socketPath, configPath string, startSession bool) error {
		steps = append(steps, "ensure:"+workspace.OrchestratorName(node.Prefix))
		if node == nil {
			return fmt.Errorf("missing project node")
		}
		if parentName != "orchestrator" {
			return fmt.Errorf("unexpected parent %q", parentName)
		}
		if socketPath != "/tmp/ax.sock" {
			return fmt.Errorf("unexpected socket path %q", socketPath)
		}
		if configPath != rootConfigPath {
			return fmt.Errorf("unexpected config path %q", configPath)
		}
		if !startSession {
			return fmt.Errorf("expected startSession=true")
		}
		sessionExists = true
		return nil
	}

	client := NewDaemonClient("/tmp/ax.sock", "tester")
	result, err := agentLifecycleHandler(client, rootConfigPath, agentLifecycleActionRestart)(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"name": "team.orchestrator",
			},
		},
	})
	if err != nil {
		t.Fatalf("restart handler returned error: %v", err)
	}

	var payload agentLifecycleResult
	decodeToolResultJSON(t, result, &payload)

	if payload.Status != "restarted" {
		t.Fatalf("expected status restarted, got %q", payload.Status)
	}
	if payload.TargetKind != "orchestrator" {
		t.Fatalf("expected orchestrator target kind, got %q", payload.TargetKind)
	}
	if got, want := strings.Join(steps, ","), "cleanup:team.orchestrator,ensure:team.orchestrator"; got != want {
		t.Fatalf("steps = %q, want %q", got, want)
	}
	if !payload.SessionExistsBefore || !payload.SessionExistsAfter {
		t.Fatalf("expected orchestrator to exist before and after restart, got before=%v after=%v", payload.SessionExistsBefore, payload.SessionExistsAfter)
	}
}

func TestStopAgentHandlerRejectsRootOrchestrator(t *testing.T) {
	root := t.TempDir()
	rootConfigPath := filepath.Join(root, ".ax", "config.yaml")
	writeLifecycleConfig(t, rootConfigPath, `
project: root
orchestrator_runtime: claude
workspaces:
  root:
    dir: .
    runtime: claude
`)

	client := NewDaemonClient("/tmp/ax.sock", "tester")
	result, err := agentLifecycleHandler(client, rootConfigPath, agentLifecycleActionStop)(context.Background(), mcp.CallToolRequest{
		Params: mcp.CallToolParams{
			Arguments: map[string]any{
				"name": "orchestrator",
			},
		},
	})
	if err != nil {
		t.Fatalf("stop handler returned error: %v", err)
	}
	assertToolErrorContains(t, result, `does not support stop`)
	assertToolErrorContains(t, result, "root orchestrator lifecycle is not supported")
}

func stubAgentLifecycleOps(t *testing.T) func() {
	t.Helper()

	oldSessionExists := lifecycleSessionExists
	oldManagerFactory := newLifecycleWorkspaceManager
	oldEnsureOrchestrator := ensureLifecycleOrchestrator
	oldCleanupOrchestrator := cleanupLifecycleOrchestratorState

	restore := func() {
		lifecycleSessionExists = oldSessionExists
		newLifecycleWorkspaceManager = oldManagerFactory
		ensureLifecycleOrchestrator = oldEnsureOrchestrator
		cleanupLifecycleOrchestratorState = oldCleanupOrchestrator
	}
	t.Cleanup(restore)
	return restore
}

func writeLifecycleConfig(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir %s: %v", filepath.Dir(path), err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func assertToolErrorContains(t *testing.T, result *mcp.CallToolResult, want string) {
	t.Helper()
	if result == nil {
		t.Fatal("expected tool result, got nil")
	}
	if !result.IsError {
		t.Fatalf("expected tool error result, got success: %+v", result.Content)
	}
	if len(result.Content) != 1 {
		t.Fatalf("expected one content item, got %d", len(result.Content))
	}
	text, ok := result.Content[0].(mcp.TextContent)
	if !ok {
		t.Fatalf("expected text content, got %T", result.Content[0])
	}
	if !strings.Contains(text.Text, want) {
		t.Fatalf("expected tool error %q to contain %q", text.Text, want)
	}
}
