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
