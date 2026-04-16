package mcpserver

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"testing"

	"github.com/ashon/ax/internal/daemon"
	axmemory "github.com/ashon/ax/internal/memory"
	"github.com/ashon/ax/internal/types"
)

func TestRememberAndRecallMemoriesRoundTrip(t *testing.T) {
	root := t.TempDir()
	cfgPath := writeMCPTeamConfig(t, root, `
project: demo
workspaces:
  main:
    dir: .
`)

	client, serverErr := newMemoryToolTestClient(t, "orchestrator")

	rememberResult, err := rememberMemoryHandler(client, cfgPath)(context.Background(), toolRequest("remember_memory", map[string]any{
		"scope":   "project",
		"kind":    "decision",
		"subject": "Release",
		"content": "Use feature flags for every risky rollout.",
		"tags":    []string{"release", "ops"},
	}))
	if err != nil {
		t.Fatalf("remember_memory returned error: %v", err)
	}
	var remembered types.Memory
	decodeToolResultJSON(t, rememberResult, &remembered)
	if remembered.Scope != axmemory.ProjectScope("") {
		t.Fatalf("scope=%q, want %q", remembered.Scope, axmemory.ProjectScope(""))
	}
	if remembered.Kind != "decision" {
		t.Fatalf("kind=%q, want decision", remembered.Kind)
	}

	recallResult, err := recallMemoriesHandler(client, cfgPath)(context.Background(), toolRequest("recall_memories", map[string]any{
		"scopes": []string{"project"},
	}))
	if err != nil {
		t.Fatalf("recall_memories returned error: %v", err)
	}
	var payload struct {
		Scopes   []string       `json:"scopes"`
		Count    int            `json:"count"`
		Memories []types.Memory `json:"memories"`
	}
	decodeToolResultJSON(t, recallResult, &payload)
	if payload.Count != 1 || len(payload.Memories) != 1 {
		t.Fatalf("expected one recalled memory, got %+v", payload)
	}
	if payload.Scopes[0] != axmemory.ProjectScope("") {
		t.Fatalf("unexpected recall scope resolution: %+v", payload.Scopes)
	}
	if payload.Memories[0].ID != remembered.ID {
		t.Fatalf("expected recalled memory %q, got %+v", remembered.ID, payload.Memories)
	}
	if err := <-serverErr; err != nil {
		t.Fatalf("daemon stub failed: %v", err)
	}
}

func TestRecallMemoriesDefaultsToGlobalProjectAndWorkspaceScopes(t *testing.T) {
	root := t.TempDir()
	cfgPath := writeMCPTeamConfig(t, root, `
project: demo
workspaces:
  main:
    dir: .
`)

	client, serverErr := newMemoryToolTestClient(t, "orchestrator")

	if _, err := client.RememberMemory(axmemory.GlobalScope, "constraint", "", "Never bypass CI before merge.", []string{"ci"}, nil); err != nil {
		t.Fatalf("remember global: %v", err)
	}
	if _, err := client.RememberMemory(axmemory.WorkspaceScope("orchestrator"), "handoff", "", "User cares about rollout safety.", []string{"user"}, nil); err != nil {
		t.Fatalf("remember workspace: %v", err)
	}

	result, err := recallMemoriesHandler(client, cfgPath)(context.Background(), toolRequest("recall_memories", map[string]any{}))
	if err != nil {
		t.Fatalf("recall_memories returned error: %v", err)
	}
	var payload struct {
		Scopes   []string       `json:"scopes"`
		Count    int            `json:"count"`
		Memories []types.Memory `json:"memories"`
	}
	decodeToolResultJSON(t, result, &payload)
	if len(payload.Scopes) != 3 {
		t.Fatalf("expected default scopes, got %+v", payload.Scopes)
	}
	if payload.Count != 2 || len(payload.Memories) != 2 {
		t.Fatalf("expected global and workspace memories, got %+v", payload)
	}
	if err := <-serverErr; err != nil {
		t.Fatalf("daemon stub failed: %v", err)
	}
}

func TestSupersedeMemoryAndListMemoriesIncludeSupersededEntries(t *testing.T) {
	root := t.TempDir()
	cfgPath := writeMCPTeamConfig(t, root, `
project: demo
workspaces:
  main:
    dir: .
`)

	client, serverErr := newMemoryToolTestClient(t, "orchestrator")

	original, err := client.RememberMemory(axmemory.ProjectScope(""), "decision", "Release", "Ship manually on Fridays.", []string{"release"}, nil)
	if err != nil {
		t.Fatalf("remember original: %v", err)
	}

	supersedeResult, err := supersedeMemoryHandler(client, cfgPath)(context.Background(), toolRequest("supersede_memory", map[string]any{
		"scope":          "project",
		"kind":           "decision",
		"subject":        "Release",
		"content":        "Use staged rollout automation instead of manual Friday releases.",
		"tags":           []string{"release"},
		"supersedes_ids": []string{original.ID},
	}))
	if err != nil {
		t.Fatalf("supersede_memory returned error: %v", err)
	}
	var replacement types.Memory
	decodeToolResultJSON(t, supersedeResult, &replacement)
	if len(replacement.Supersedes) != 1 || replacement.Supersedes[0] != original.ID {
		t.Fatalf("expected replacement to supersede %q, got %+v", original.ID, replacement)
	}

	listResult, err := listMemoriesHandler(client, cfgPath)(context.Background(), toolRequest("list_memories", map[string]any{
		"scopes":             []string{"project"},
		"include_superseded": true,
	}))
	if err != nil {
		t.Fatalf("list_memories returned error: %v", err)
	}
	var payload struct {
		Count    int            `json:"count"`
		Memories []types.Memory `json:"memories"`
	}
	decodeToolResultJSON(t, listResult, &payload)
	if payload.Count != 2 || len(payload.Memories) != 2 {
		t.Fatalf("expected original + replacement memories, got %+v", payload)
	}
	if payload.Memories[0].ID != replacement.ID {
		t.Fatalf("expected active replacement first, got %+v", payload.Memories)
	}
	var foundSuperseded bool
	for _, entry := range payload.Memories {
		if entry.ID == original.ID {
			foundSuperseded = true
			if entry.SupersededBy != replacement.ID || entry.SupersededAt == nil {
				t.Fatalf("expected original memory to be superseded by %q, got %+v", replacement.ID, entry)
			}
		}
	}
	if !foundSuperseded {
		t.Fatalf("expected superseded original memory %q in %+v", original.ID, payload.Memories)
	}
	if err := <-serverErr; err != nil {
		t.Fatalf("daemon stub failed: %v", err)
	}
}

func newMemoryToolTestClient(t *testing.T, workspaceName string) (*DaemonClient, <-chan error) {
	t.Helper()

	clientConn, serverConn := net.Pipe()
	client := NewDaemonClient("", workspaceName)
	client.conn = clientConn
	client.connected.Store(true)
	client.setDisconnectErr(nil)
	go client.readLoop()

	store := axmemory.NewStore(t.TempDir())
	serverErr := make(chan error, 1)
	go func() {
		defer close(serverErr)
		defer serverConn.Close()

		scanner := bufio.NewScanner(serverConn)
		handledRecall := false
		for scanner.Scan() {
			var env daemon.Envelope
			if err := json.Unmarshal(scanner.Bytes(), &env); err != nil {
				serverErr <- fmt.Errorf("decode request: %w", err)
				return
			}

			var resp *daemon.Envelope
			var err error
			switch env.Type {
			case daemon.MsgGetTeamState:
				resp, err = daemon.NewResponseEnvelope(env.ID, &daemon.TeamStateResponse{})
			case daemon.MsgRememberMemory:
				var payload daemon.RememberMemoryPayload
				if err := env.DecodePayload(&payload); err != nil {
					serverErr <- fmt.Errorf("decode remember_memory payload: %w", err)
					return
				}
				entry, rememberErr := store.Remember(payload.Scope, payload.Kind, payload.Subject, payload.Content, payload.Tags, workspaceName, payload.Supersedes)
				if rememberErr != nil {
					resp, err = daemon.NewErrorEnvelope(env.ID, rememberErr.Error())
				} else {
					resp, err = daemon.NewResponseEnvelope(env.ID, &daemon.MemoryResponse{Memory: *entry})
				}
			case daemon.MsgRecallMemories:
				var payload daemon.RecallMemoriesPayload
				if err := env.DecodePayload(&payload); err != nil {
					serverErr <- fmt.Errorf("decode recall_memories payload: %w", err)
					return
				}
				memories, listErr := store.List(axmemory.Query{
					Scopes:            payload.Scopes,
					Kind:              payload.Kind,
					Tags:              payload.Tags,
					IncludeSuperseded: payload.IncludeSuperseded,
					Limit:             payload.Limit,
				})
				if listErr != nil {
					resp, err = daemon.NewErrorEnvelope(env.ID, listErr.Error())
				} else {
					resp, err = daemon.NewResponseEnvelope(env.ID, &daemon.RecallMemoriesResponse{Memories: memories})
				}
				handledRecall = true
			default:
				serverErr <- fmt.Errorf("unexpected request type %s", env.Type)
				return
			}
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
			if handledRecall {
				break
			}
		}
		serverErr <- nil
	}()

	t.Cleanup(func() {
		_ = client.Close()
	})

	return client, serverErr
}
