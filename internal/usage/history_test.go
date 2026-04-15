package usage

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestQueryHistory_UsesWorkspaceHintAndSessionForAttribution(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	workspaceDir := filepath.Join(home, "work", "ax")
	if err := os.MkdirAll(workspaceDir, 0o755); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}
	projectDir, err := ProjectPath(workspaceDir)
	if err != nil {
		t.Fatalf("project path: %v", err)
	}
	if err := os.MkdirAll(projectDir, 0o755); err != nil {
		t.Fatalf("mkdir project dir: %v", err)
	}

	mainPath := filepath.Join(projectDir, "sess-1.jsonl")
	subagentDir := filepath.Join(projectDir, "sess-1", "subagents")
	if err := os.MkdirAll(subagentDir, 0o755); err != nil {
		t.Fatalf("mkdir subagents: %v", err)
	}
	subPath := filepath.Join(subagentDir, "agent-agent-123.jsonl")

	attachment := fmt.Sprintf(`{"type":"attachment","timestamp":"2026-04-11T05:45:20Z","sessionId":"sess-1","cwd":"%s","attachment":{"type":"mcp_instructions_delta","addedBlocks":["## ax\nYou are the \"ax.daemon\" workspace agent in an ax multi-agent environment."]}}`, workspaceDir)
	mainUsage1 := fmt.Sprintf(`{"type":"assistant","timestamp":"2026-04-11T05:45:28Z","sessionId":"sess-1","cwd":"%s","message":{"role":"assistant","model":"claude-opus-4-6","usage":{"input_tokens":3,"output_tokens":295,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}}`, workspaceDir)
	mainUsage2 := fmt.Sprintf(`{"type":"assistant","timestamp":"2026-04-11T05:50:28Z","sessionId":"sess-1","cwd":"%s","message":{"role":"assistant","model":"claude-sonnet-4-5","usage":{"input_tokens":5,"output_tokens":120,"cache_read_input_tokens":7,"cache_creation_input_tokens":3}}}`, workspaceDir)
	subUsage := fmt.Sprintf(`{"type":"assistant","timestamp":"2026-04-11T05:46:28Z","sessionId":"sess-1","agentId":"agent-123","cwd":"%s","message":{"role":"assistant","model":"claude-opus-4-6","usage":{"input_tokens":1,"output_tokens":40,"cache_read_input_tokens":2,"cache_creation_input_tokens":1}}}`, workspaceDir)

	writeFile(t, mainPath, attachment+"\n"+mainUsage1+"\n"+mainUsage2+"\n")
	writeFile(t, subPath, subUsage+"\n")

	resp, err := QueryHistory([]WorkspaceBinding{
		{Name: "ax.cli", Dir: workspaceDir},
		{Name: "ax.daemon", Dir: workspaceDir},
	}, HistoryQuery{
		Since:      time.Date(2026, 4, 11, 5, 40, 0, 0, time.UTC),
		Until:      time.Date(2026, 4, 11, 6, 0, 0, 0, time.UTC),
		BucketSize: 5 * time.Minute,
	})
	if err != nil {
		t.Fatalf("QueryHistory: %v", err)
	}
	if len(resp.Workspaces) != 2 {
		t.Fatalf("workspaces=%d, want 2", len(resp.Workspaces))
	}

	var daemonWS, cliWS WorkspaceHistory
	for _, ws := range resp.Workspaces {
		switch ws.Workspace {
		case "ax.daemon":
			daemonWS = ws
		case "ax.cli":
			cliWS = ws
		}
	}

	if !daemonWS.Available {
		t.Fatalf("daemon workspace should be available: %+v", daemonWS)
	}
	if len(daemonWS.Agents) != 2 {
		t.Fatalf("daemon agents=%d, want 2", len(daemonWS.Agents))
	}
	if len(daemonWS.RecentBuckets) != 2 {
		t.Fatalf("daemon buckets=%d, want 2", len(daemonWS.RecentBuckets))
	}
	if daemonWS.RecentBuckets[0].Turns != 2 {
		t.Fatalf("first bucket turns=%d, want 2", daemonWS.RecentBuckets[0].Turns)
	}
	if daemonWS.RecentBuckets[0].Total != (3 + 295 + 10 + 5 + 1 + 40 + 2 + 1) {
		t.Fatalf("first bucket total=%d", daemonWS.RecentBuckets[0].Total)
	}
	if daemonWS.RecentBuckets[0].MCPProxy.PromptSignals != 1 {
		t.Fatalf("first bucket MCP prompt signals=%d, want 1", daemonWS.RecentBuckets[0].MCPProxy.PromptSignals)
	}
	if daemonWS.CurrentSnapshot.CumulativeMCPProxy.PromptSignals != 1 {
		t.Fatalf("workspace cumulative MCP prompt signals=%d, want 1", daemonWS.CurrentSnapshot.CumulativeMCPProxy.PromptSignals)
	}
	if daemonWS.CurrentSnapshot.CurrentTotal != (5 + 120 + 7 + 3 + 1 + 40 + 2 + 1) {
		t.Fatalf("workspace current total=%d", daemonWS.CurrentSnapshot.CurrentTotal)
	}

	if cliWS.Available {
		t.Fatalf("cli workspace should be unavailable: %+v", cliWS)
	}
	if cliWS.UnavailableReason != "workspace_unattributed" {
		t.Fatalf("cli unavailable reason=%q", cliWS.UnavailableReason)
	}
}

func TestQueryHistory_DedupesRepeatedAssistantRequestFrames(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	workspaceDir := filepath.Join(home, "work", "ax")
	if err := os.MkdirAll(workspaceDir, 0o755); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}
	projectDir, err := ProjectPath(workspaceDir)
	if err != nil {
		t.Fatalf("project path: %v", err)
	}
	if err := os.MkdirAll(projectDir, 0o755); err != nil {
		t.Fatalf("mkdir project dir: %v", err)
	}

	path := filepath.Join(projectDir, "sess-1.jsonl")
	thinking := fmt.Sprintf(`{"type":"assistant","timestamp":"2026-04-11T05:45:28Z","sessionId":"sess-1","cwd":"%s","requestId":"req-1","message":{"id":"msg-1","role":"assistant","model":"claude-opus-4-6","content":[{"type":"thinking","thinking":""}],"usage":{"input_tokens":3,"output_tokens":26,"cache_read_input_tokens":100,"cache_creation_input_tokens":0}}}`, workspaceDir)
	tool := fmt.Sprintf(`{"type":"assistant","timestamp":"2026-04-11T05:45:29Z","sessionId":"sess-1","cwd":"%s","requestId":"req-1","message":{"id":"msg-1","role":"assistant","model":"claude-opus-4-6","content":[{"type":"tool_use","name":"mcp__ax__read_messages","input":{}}],"usage":{"input_tokens":3,"output_tokens":26,"cache_read_input_tokens":100,"cache_creation_input_tokens":0}}}`, workspaceDir)
	final := fmt.Sprintf(`{"type":"assistant","timestamp":"2026-04-11T05:45:30Z","sessionId":"sess-1","cwd":"%s","requestId":"req-1","message":{"id":"msg-1","role":"assistant","model":"claude-opus-4-6","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":3,"output_tokens":40,"cache_read_input_tokens":100,"cache_creation_input_tokens":0}}}`, workspaceDir)
	writeFile(t, path, thinking+"\n"+tool+"\n"+final+"\n")

	resp, err := QueryHistory([]WorkspaceBinding{{Name: "ax.daemon", Dir: workspaceDir}}, HistoryQuery{
		Since:      time.Date(2026, 4, 11, 5, 40, 0, 0, time.UTC),
		Until:      time.Date(2026, 4, 11, 6, 0, 0, 0, time.UTC),
		BucketSize: 5 * time.Minute,
	})
	if err != nil {
		t.Fatalf("QueryHistory: %v", err)
	}
	if len(resp.Workspaces) != 1 {
		t.Fatalf("workspaces=%d, want 1", len(resp.Workspaces))
	}
	ws := resp.Workspaces[0]
	if !ws.Available {
		t.Fatalf("workspace unavailable: %+v", ws)
	}
	if len(ws.RecentBuckets) != 1 {
		t.Fatalf("buckets=%d, want 1", len(ws.RecentBuckets))
	}
	b := ws.RecentBuckets[0]
	if b.Turns != 1 {
		t.Fatalf("bucket turns=%d, want 1", b.Turns)
	}
	if b.Total != 143 {
		t.Fatalf("bucket total=%d, want 143", b.Total)
	}
	if b.MCPProxy.ToolUseTokens != 143 || b.MCPProxy.ToolUseTurns != 1 {
		t.Fatalf("bucket MCP=%+v, want tokens=143 turns=1", b.MCPProxy)
	}
}
