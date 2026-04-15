package usage

import "testing"

const (
	lineUser   = `{"type":"user","timestamp":"2026-04-11T05:45:25Z","sessionId":"sess-1","cwd":"/tmp/x","message":{"role":"user","content":"hi"}}`
	lineMCPAtt = `{"type":"attachment","timestamp":"2026-04-11T05:45:26Z","sessionId":"sess-1","cwd":"/tmp/x","attachment":{"type":"mcp_instructions_delta","addedBlocks":["## ax\nYou are the \"ax.daemon\" workspace agent."]}}`
	lineOpus1  = `{"type":"assistant","timestamp":"2026-04-11T05:45:28Z","sessionId":"sess-1","cwd":"/tmp/x","message":{"role":"assistant","model":"claude-opus-4-6","usage":{"input_tokens":3,"output_tokens":295,"cache_read_input_tokens":12253,"cache_creation_input_tokens":6448}}}`
	lineOpus2  = `{"type":"assistant","timestamp":"2026-04-11T05:45:32Z","sessionId":"sess-1","cwd":"/tmp/x","message":{"role":"assistant","model":"claude-opus-4-6","usage":{"input_tokens":1,"output_tokens":777,"cache_read_input_tokens":182166,"cache_creation_input_tokens":983}}}`
	lineSonnet = `{"type":"assistant","timestamp":"2026-04-11T05:45:35Z","sessionId":"sess-1","cwd":"/tmp/x","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"tool_use","name":"mcp__ax__read_messages","input":{}}],"usage":{"input_tokens":5,"output_tokens":120,"cache_read_input_tokens":500,"cache_creation_input_tokens":100}}}`
	lineBad    = `{not json`
)

func TestAggregator_Cumulative(t *testing.T) {
	a := NewAggregator()
	for _, l := range []string{lineUser, lineMCPAtt, lineOpus1, lineOpus2, lineSonnet, lineBad} {
		a.IngestLine([]byte(l))
	}
	if a.Turns() != 3 {
		t.Errorf("turns=%d, want 3", a.Turns())
	}
	if a.ParseErrors() != 1 {
		t.Errorf("parse errors=%d, want 1", a.ParseErrors())
	}
	snap := a.Snapshot("ws", "/tmp/x.jsonl")
	wantCum := Tokens{Input: 9, Output: 1192, CacheRead: 194919, CacheCreation: 7531}
	if snap.CumulativeTotals != wantCum {
		t.Errorf("cumulative=%+v, want %+v", snap.CumulativeTotals, wantCum)
	}
	// CurrentContext should reflect the last usage-bearing line (lineSonnet).
	wantCurrent := Tokens{Input: 5, Output: 120, CacheRead: 500, CacheCreation: 100}
	if snap.CurrentContext != wantCurrent {
		t.Errorf("current=%+v, want %+v", snap.CurrentContext, wantCurrent)
	}
	if snap.CumulativeMCP.PromptSignals != 1 {
		t.Fatalf("prompt signals=%d, want 1", snap.CumulativeMCP.PromptSignals)
	}
	if snap.CumulativeMCP.ToolUseTurns != 1 {
		t.Fatalf("tool use turns=%d, want 1", snap.CumulativeMCP.ToolUseTurns)
	}
	if snap.CurrentMCP.ToolUseTokens != wantCurrent.Total() {
		t.Fatalf("current MCP tool tokens=%d, want %d", snap.CurrentMCP.ToolUseTokens, wantCurrent.Total())
	}
	if snap.CurrentModel != "claude-sonnet-4-5" {
		t.Errorf("current model=%q", snap.CurrentModel)
	}
	if len(snap.ByModel) != 2 {
		t.Fatalf("by_model len=%d, want 2", len(snap.ByModel))
	}
	// ByModel sorted by name: claude-opus-4-6, claude-sonnet-4-5.
	if snap.ByModel[0].Model != "claude-opus-4-6" || snap.ByModel[0].Turns != 2 {
		t.Errorf("opus totals = %+v", snap.ByModel[0])
	}
	if snap.ByModel[1].Model != "claude-sonnet-4-5" || snap.ByModel[1].Turns != 1 {
		t.Errorf("sonnet totals = %+v", snap.ByModel[1])
	}
	if !snap.Available {
		t.Error("expected Available=true")
	}
	if snap.SessionID != "sess-1" {
		t.Errorf("session=%q", snap.SessionID)
	}
	if snap.SessionStart == nil || snap.LastActivity == nil {
		t.Error("session times missing")
	}
	if snap.SessionStart.After(*snap.LastActivity) {
		t.Error("session_start after last_activity")
	}
}

func TestAggregator_Reset(t *testing.T) {
	a := NewAggregator()
	a.IngestLine([]byte(lineOpus1))
	if a.Turns() != 1 {
		t.Fatalf("turns=%d", a.Turns())
	}
	a.Reset()
	if a.Turns() != 0 || a.SessionID() != "" || a.ParseErrors() != 0 {
		t.Errorf("reset incomplete: turns=%d session=%q errs=%d", a.Turns(), a.SessionID(), a.ParseErrors())
	}
	snap := a.Snapshot("ws", "")
	if snap.Available {
		t.Error("Available should be false after reset")
	}
	if snap.CumulativeTotals != (Tokens{}) {
		t.Errorf("cumulative should be zero, got %+v", snap.CumulativeTotals)
	}
}

func TestAggregator_DedupesRepeatedAssistantRequests(t *testing.T) {
	const (
		lineReq1Thinking = `{"type":"assistant","timestamp":"2026-04-11T05:45:28Z","sessionId":"sess-1","cwd":"/tmp/x","requestId":"req-1","message":{"id":"msg-1","role":"assistant","model":"claude-opus-4-6","content":[{"type":"thinking","thinking":""}],"usage":{"input_tokens":3,"output_tokens":26,"cache_read_input_tokens":100,"cache_creation_input_tokens":0}}}`
		lineReq1Tool     = `{"type":"assistant","timestamp":"2026-04-11T05:45:29Z","sessionId":"sess-1","cwd":"/tmp/x","requestId":"req-1","message":{"id":"msg-1","role":"assistant","model":"claude-opus-4-6","content":[{"type":"tool_use","name":"mcp__ax__read_messages","input":{}}],"usage":{"input_tokens":3,"output_tokens":26,"cache_read_input_tokens":100,"cache_creation_input_tokens":0}}}`
		lineReq1Final    = `{"type":"assistant","timestamp":"2026-04-11T05:45:30Z","sessionId":"sess-1","cwd":"/tmp/x","requestId":"req-1","message":{"id":"msg-1","role":"assistant","model":"claude-opus-4-6","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":3,"output_tokens":40,"cache_read_input_tokens":100,"cache_creation_input_tokens":0}}}`
	)

	a := NewAggregator()
	for _, line := range []string{lineReq1Thinking, lineReq1Tool, lineReq1Final} {
		if !a.IngestLine([]byte(line)) {
			t.Fatalf("expected usage-bearing line: %s", line)
		}
	}

	snap := a.Snapshot("ws", "/tmp/x.jsonl")
	if snap.Turns != 1 {
		t.Fatalf("turns=%d, want 1", snap.Turns)
	}
	wantTotals := Tokens{Input: 3, Output: 40, CacheRead: 100}
	if snap.CumulativeTotals != wantTotals {
		t.Fatalf("cumulative=%+v, want %+v", snap.CumulativeTotals, wantTotals)
	}
	if len(snap.ByModel) != 1 || snap.ByModel[0].Turns != 1 {
		t.Fatalf("by_model=%+v, want one deduped turn", snap.ByModel)
	}
	wantMCP := wantTotals.Total()
	if snap.CumulativeMCP.ToolUseTurns != 1 {
		t.Fatalf("cumulative MCP turns=%d, want 1", snap.CumulativeMCP.ToolUseTurns)
	}
	if snap.CumulativeMCP.ToolUseTokens != wantMCP {
		t.Fatalf("cumulative MCP tokens=%d, want %d", snap.CumulativeMCP.ToolUseTokens, wantMCP)
	}
	if snap.CurrentMCP.ToolUseTokens != wantMCP {
		t.Fatalf("current MCP tokens=%d, want %d", snap.CurrentMCP.ToolUseTokens, wantMCP)
	}
}
