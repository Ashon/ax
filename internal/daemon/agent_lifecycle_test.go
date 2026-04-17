package daemon

import (
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/workspace"
)

type stubAgentWorkspaceManager struct {
	createFn  func(name string, ws config.Workspace) error
	restartFn func(name string, ws config.Workspace) error
	destroyFn func(name, dir string) error
}

func (m stubAgentWorkspaceManager) Create(name string, ws config.Workspace) error {
	if m.createFn == nil {
		return nil
	}
	return m.createFn(name, ws)
}

func (m stubAgentWorkspaceManager) Restart(name string, ws config.Workspace) error {
	if m.restartFn == nil {
		return nil
	}
	return m.restartFn(name, ws)
}

func (m stubAgentWorkspaceManager) Destroy(name, dir string) error {
	if m.destroyFn == nil {
		return nil
	}
	return m.destroyFn(name, dir)
}

func writeAgentLifecycleConfig(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir %s: %v", filepath.Dir(path), err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func newAgentLifecycleDaemon(t *testing.T, ops *agentLifecycleOps) *Daemon {
	t.Helper()

	stateDir := t.TempDir()
	d := &Daemon{
		socketPath: "/tmp/ax.sock",
		registry:   NewRegistry(),
		taskStore:  NewTaskStore(stateDir),
		logger:     log.New(io.Discard, "", 0),
		agentOps:   ops,
	}
	clientConn, serverConn := net.Pipe()
	t.Cleanup(func() {
		clientConn.Close()
		serverConn.Close()
	})
	d.registry.Register("tester", "", "", "", 0, clientConn)
	return d
}

func decodeAgentLifecycleResponse(t *testing.T, env *Envelope) AgentLifecycleResponse {
	t.Helper()
	var payload ResponsePayload
	if err := env.DecodePayload(&payload); err != nil {
		t.Fatalf("decode response payload: %v", err)
	}
	var result AgentLifecycleResponse
	if err := json.Unmarshal(payload.Data, &result); err != nil {
		t.Fatalf("decode agent lifecycle response: %v", err)
	}
	return result
}

func TestHandleAgentLifecycleStartsWorkspaceByExactName(t *testing.T) {
	root := t.TempDir()
	cfgPath := filepath.Join(root, ".ax", "config.yaml")
	writeAgentLifecycleConfig(t, cfgPath, `
project: demo
workspaces:
  worker:
    dir: ./worker
    runtime: claude
`)

	sessionExists := false
	ops := &agentLifecycleOps{
		newManager: func(socketPath, configPath string) agentWorkspaceManager {
			return stubAgentWorkspaceManager{
				createFn: func(name string, ws config.Workspace) error {
					if name != "worker" {
						return fmt.Errorf("unexpected workspace %q", name)
					}
					expectedDir := filepath.Join(root, "worker")
					if ws.Dir != expectedDir {
						return fmt.Errorf("unexpected workspace dir %q (want %q)", ws.Dir, expectedDir)
					}
					sessionExists = true
					return nil
				},
			}
		},
		ensureOrchestrator:  func(node *config.ProjectNode, parentName, socketPath, configPath string, startSession bool) error { return nil },
		cleanupOrchestrator: func(name, artifactDir string) error { return nil },
		sessionExists: func(name string) bool {
			return name == "worker" && sessionExists
		},
	}
	d := newAgentLifecycleDaemon(t, ops)

	env, _ := NewEnvelope("al-1", MsgAgentLifecycle, &AgentLifecyclePayload{
		ConfigPath: cfgPath,
		Name:       "worker",
		Action:     types.LifecycleActionStart,
	})
	resp, err := d.handleAgentLifecycleEnvelope(env, "tester")
	if err != nil {
		t.Fatalf("handle agent_lifecycle: %v", err)
	}
	result := decodeAgentLifecycleResponse(t, resp)

	if result.Status != "started" {
		t.Fatalf("expected status started, got %q", result.Status)
	}
	if result.TargetKind != "workspace" {
		t.Fatalf("expected workspace target kind, got %q", result.TargetKind)
	}
	if result.SessionExistsBefore {
		t.Fatal("expected workspace to start from a stopped state")
	}
	if !result.SessionExistsAfter {
		t.Fatal("expected workspace session after start")
	}
	if !result.ExactMatch {
		t.Fatal("expected exact_match to be true")
	}
}

func TestHandleAgentLifecycleRejectsUnknownName(t *testing.T) {
	root := t.TempDir()
	cfgPath := filepath.Join(root, ".ax", "config.yaml")
	writeAgentLifecycleConfig(t, cfgPath, `
project: demo
workspaces:
  worker:
    dir: ./worker
    runtime: claude
`)

	ops := defaultAgentLifecycleOps()
	d := newAgentLifecycleDaemon(t, ops)

	env, _ := NewEnvelope("al-2", MsgAgentLifecycle, &AgentLifecyclePayload{
		ConfigPath: cfgPath,
		Name:       "work",
		Action:     types.LifecycleActionStart,
	})
	_, err := d.handleAgentLifecycleEnvelope(env, "tester")
	if err == nil {
		t.Fatal("expected error for unknown agent name")
	}
	if !strings.Contains(err.Error(), `Agent "work" is not defined exactly`) {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestHandleAgentLifecycleRestartsManagedChildOrchestrator(t *testing.T) {
	root := t.TempDir()
	childDir := filepath.Join(root, "child")
	rootConfigPath := filepath.Join(root, ".ax", "config.yaml")
	writeAgentLifecycleConfig(t, rootConfigPath, `
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
	writeAgentLifecycleConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
project: child
orchestrator_runtime: claude
workspaces:
  dev:
    dir: .
    runtime: claude
`)

	sessionExists := true
	var steps []string
	ops := &agentLifecycleOps{
		newManager: func(socketPath, configPath string) agentWorkspaceManager {
			return stubAgentWorkspaceManager{}
		},
		cleanupOrchestrator: func(name, dir string) error {
			steps = append(steps, "cleanup:"+name)
			if name != "team.orchestrator" {
				return fmt.Errorf("unexpected cleanup target %q", name)
			}
			expectedDir := filepath.Join(childDir, ".ax", "orchestrator-team")
			if dir != expectedDir {
				return fmt.Errorf("unexpected artifact dir %q (want %q)", dir, expectedDir)
			}
			sessionExists = false
			return nil
		},
		ensureOrchestrator: func(node *config.ProjectNode, parentName, socketPath, configPath string, startSession bool) error {
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
		},
		sessionExists: func(name string) bool {
			return name == "team.orchestrator" && sessionExists
		},
	}
	d := newAgentLifecycleDaemon(t, ops)

	env, _ := NewEnvelope("al-3", MsgAgentLifecycle, &AgentLifecyclePayload{
		ConfigPath: rootConfigPath,
		Name:       "team.orchestrator",
		Action:     types.LifecycleActionRestart,
	})
	resp, err := d.handleAgentLifecycleEnvelope(env, "tester")
	if err != nil {
		t.Fatalf("handle agent_lifecycle: %v", err)
	}
	result := decodeAgentLifecycleResponse(t, resp)

	if result.Status != "restarted" {
		t.Fatalf("expected status restarted, got %q", result.Status)
	}
	if result.TargetKind != "orchestrator" {
		t.Fatalf("expected orchestrator target kind, got %q", result.TargetKind)
	}
	if got, want := strings.Join(steps, ","), "cleanup:team.orchestrator,ensure:team.orchestrator"; got != want {
		t.Fatalf("steps = %q, want %q", got, want)
	}
	if !result.SessionExistsBefore || !result.SessionExistsAfter {
		t.Fatalf("expected orchestrator to exist before and after restart, got before=%v after=%v", result.SessionExistsBefore, result.SessionExistsAfter)
	}
}

func TestHandleAgentLifecycleRejectsRootOrchestrator(t *testing.T) {
	root := t.TempDir()
	rootConfigPath := filepath.Join(root, ".ax", "config.yaml")
	writeAgentLifecycleConfig(t, rootConfigPath, `
project: root
orchestrator_runtime: claude
workspaces:
  root:
    dir: .
    runtime: claude
`)

	d := newAgentLifecycleDaemon(t, defaultAgentLifecycleOps())

	env, _ := NewEnvelope("al-4", MsgAgentLifecycle, &AgentLifecyclePayload{
		ConfigPath: rootConfigPath,
		Name:       "orchestrator",
		Action:     types.LifecycleActionStop,
	})
	_, err := d.handleAgentLifecycleEnvelope(env, "tester")
	if err == nil {
		t.Fatal("expected error rejecting root orchestrator lifecycle")
	}
	if !strings.Contains(err.Error(), "does not support stop") {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(err.Error(), "root orchestrator lifecycle is not supported") {
		t.Fatalf("unexpected error: %v", err)
	}
}
