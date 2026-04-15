package usage

import "testing"

func TestParseLine_AssistantWithUsage(t *testing.T) {
	line := []byte(`{"type":"assistant","timestamp":"2026-04-11T05:45:28.434Z","sessionId":"sess-1","cwd":"/tmp/x","message":{"role":"assistant","model":"claude-opus-4-6","usage":{"input_tokens":3,"output_tokens":295,"cache_read_input_tokens":12253,"cache_creation_input_tokens":6448}}}`)
	rec, err := parseLine(line)
	if err != nil {
		t.Fatalf("parseLine: %v", err)
	}
	if !rec.HasUsage {
		t.Fatal("expected HasUsage=true")
	}
	if rec.Model != "claude-opus-4-6" {
		t.Errorf("model=%q", rec.Model)
	}
	if rec.SessionID != "sess-1" || rec.Cwd != "/tmp/x" {
		t.Errorf("session/cwd: %q %q", rec.SessionID, rec.Cwd)
	}
	want := Tokens{Input: 3, Output: 295, CacheRead: 12253, CacheCreation: 6448}
	if rec.Tokens != want {
		t.Errorf("tokens = %+v, want %+v", rec.Tokens, want)
	}
	if rec.Timestamp.IsZero() {
		t.Error("timestamp missing")
	}
}

func TestParseLine_UserNoUsage(t *testing.T) {
	line := []byte(`{"type":"user","timestamp":"2026-04-11T05:45:25.103Z","sessionId":"sess-1","cwd":"/tmp/x","message":{"role":"user","content":"hi"}}`)
	rec, err := parseLine(line)
	if err != nil {
		t.Fatalf("parseLine: %v", err)
	}
	if rec.HasUsage {
		t.Error("user line should not have usage")
	}
	if rec.SessionID != "sess-1" {
		t.Errorf("session propagation failed: %q", rec.SessionID)
	}
}

func TestParseLine_UnknownFieldsIgnored(t *testing.T) {
	// Extra fields (service_tier, iterations, cache_creation subobject) must
	// not prevent parsing.
	line := []byte(`{"type":"assistant","sessionId":"s","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4,"service_tier":"standard","iterations":[{"input_tokens":1}]}},"extra":42}`)
	rec, err := parseLine(line)
	if err != nil {
		t.Fatalf("parseLine: %v", err)
	}
	if rec.Tokens.Input != 1 || rec.Tokens.Output != 2 || rec.Tokens.CacheRead != 3 || rec.Tokens.CacheCreation != 4 {
		t.Errorf("tokens=%+v", rec.Tokens)
	}
}

func TestParseLine_Malformed(t *testing.T) {
	_, err := parseLine([]byte(`{not json`))
	if err == nil {
		t.Error("expected error on malformed json")
	}
}

func TestParseLine_PermissionMode(t *testing.T) {
	// The first line of real transcripts lacks "message" entirely.
	line := []byte(`{"type":"permission-mode","permissionMode":"default","sessionId":"s"}`)
	rec, err := parseLine(line)
	if err != nil {
		t.Fatalf("parseLine: %v", err)
	}
	if rec.HasUsage {
		t.Error("permission-mode line must not report usage")
	}
}

func TestParseLine_TracksMCPInstructionProxy(t *testing.T) {
	line := []byte(`{"type":"attachment","timestamp":"2026-04-11T05:45:20Z","sessionId":"sess-1","cwd":"/tmp/x","attachment":{"type":"mcp_instructions_delta","addedBlocks":["## ax\nYou are the \"ax.daemon\" workspace agent."]}}`)
	rec, err := parseLine(line)
	if err != nil {
		t.Fatalf("parseLine: %v", err)
	}
	if rec.MCPProxy.PromptSignals != 1 {
		t.Fatalf("prompt signals=%d, want 1", rec.MCPProxy.PromptSignals)
	}
	if rec.MCPProxy.PromptTokens == 0 || rec.MCPProxy.Total == 0 {
		t.Fatalf("expected non-zero prompt proxy, got %+v", rec.MCPProxy)
	}
}

func TestParseLine_TracksMCPToolUseTokens(t *testing.T) {
	line := []byte(`{"type":"assistant","timestamp":"2026-04-11T05:45:28Z","sessionId":"sess-1","cwd":"/tmp/x","message":{"role":"assistant","model":"claude-opus-4-6","content":[{"type":"tool_use","name":"mcp__ax__read_messages","input":{}}],"usage":{"input_tokens":3,"output_tokens":7,"cache_read_input_tokens":11,"cache_creation_input_tokens":13}}}`)
	rec, err := parseLine(line)
	if err != nil {
		t.Fatalf("parseLine: %v", err)
	}
	if rec.MCPProxy.ToolUseTurns != 1 {
		t.Fatalf("tool_use_turns=%d, want 1", rec.MCPProxy.ToolUseTurns)
	}
	if rec.MCPProxy.ToolUseTokens != rec.Tokens.Total() {
		t.Fatalf("tool_use_tokens=%d, want %d", rec.MCPProxy.ToolUseTokens, rec.Tokens.Total())
	}
}
