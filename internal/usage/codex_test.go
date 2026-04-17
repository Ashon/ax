package usage

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/ashon/ax/internal/agent"
)

const codexSessionFixture = `{"timestamp":"2026-04-14T15:25:54.762Z","type":"session_meta","payload":{"id":"session-abc","timestamp":"2026-04-14T15:21:09.246Z","cwd":"/workspace/dir","originator":"codex-tui"}}
{"timestamp":"2026-04-14T15:25:55.005Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex"}}}
{"timestamp":"2026-04-14T15:26:26.715Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":24558,"cached_input_tokens":5504,"output_tokens":634,"reasoning_output_tokens":516,"total_tokens":25192},"last_token_usage":{"input_tokens":24558,"cached_input_tokens":5504,"output_tokens":634,"reasoning_output_tokens":516,"total_tokens":25192},"model_context_window":258400},"rate_limits":null}}
{"timestamp":"2026-04-14T15:27:12.101Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50974,"cached_input_tokens":8960,"output_tokens":1556,"reasoning_output_tokens":1032,"total_tokens":52530},"last_token_usage":{"input_tokens":26416,"cached_input_tokens":3456,"output_tokens":922,"reasoning_output_tokens":516,"total_tokens":27338},"model_context_window":258400},"rate_limits":null}}
`

func writeCodexSession(t *testing.T, dir, content string) string {
	t.Helper()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatalf("mkdir %s: %v", dir, err)
	}
	path := filepath.Join(dir, "rollout-2026-04-14T15-21-09-session.jsonl")
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
	return path
}

func TestScanCodexTranscript_ProducesPerTurnBuckets(t *testing.T) {
	sessionsDir := filepath.Join(t.TempDir(), "sessions", "2026", "04", "14")
	path := writeCodexSession(t, sessionsDir, codexSessionFixture)

	since, _ := time.Parse(time.RFC3339, "2026-04-14T00:00:00Z")
	until, _ := time.Parse(time.RFC3339, "2026-04-15T00:00:00Z")
	series, err := scanCodexTranscript(path, HistoryQuery{Since: since, Until: until, BucketSize: 5 * time.Minute})
	if err != nil {
		t.Fatalf("scan codex transcript: %v", err)
	}
	if series == nil {
		t.Fatal("expected series, got nil")
	}
	if series.sessionID != "session-abc" {
		t.Fatalf("sessionID = %q, want session-abc", series.sessionID)
	}
	if series.cwd != "/workspace/dir" {
		t.Fatalf("cwd = %q, want /workspace/dir", series.cwd)
	}
	if series.agent != codexAgentName {
		t.Fatalf("agent = %q, want %q", series.agent, codexAgentName)
	}
	// Two token_count events with populated info; the null-info rate-limit
	// pulse must not count as a turn.
	if series.current.Turns != 2 {
		t.Fatalf("turns = %d, want 2", series.current.Turns)
	}
	// Cumulative input is sum of last_token_usage.input - cached across turns:
	// (24558-5504) + (26416-3456) = 19054 + 22960 = 42014.
	if series.current.CumulativeTotals.Input != 42014 {
		t.Fatalf("cumulative input = %d, want 42014", series.current.CumulativeTotals.Input)
	}
	// Cumulative cache read = 5504 + 3456 = 8960.
	if series.current.CumulativeTotals.CacheRead != 8960 {
		t.Fatalf("cumulative cache read = %d, want 8960", series.current.CumulativeTotals.CacheRead)
	}
	// Cumulative output = 634 + 922 = 1556.
	if series.current.CumulativeTotals.Output != 1556 {
		t.Fatalf("cumulative output = %d, want 1556", series.current.CumulativeTotals.Output)
	}
	if len(series.buckets) == 0 {
		t.Fatal("expected at least one bucket from token_count events inside query window")
	}
}

func TestScanCodexTranscript_SkipsNullInfoTokenCounts(t *testing.T) {
	nullOnly := `{"timestamp":"2026-04-14T15:25:54.762Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-04-14T15:21:09.246Z","cwd":"/x"}}
{"timestamp":"2026-04-14T15:25:55.005Z","type":"event_msg","payload":{"type":"token_count","info":null}}
`
	sessionsDir := filepath.Join(t.TempDir(), "sessions", "2026", "04", "14")
	path := writeCodexSession(t, sessionsDir, nullOnly)

	series, err := scanCodexTranscript(path, HistoryQuery{BucketSize: 5 * time.Minute, Until: time.Now().Add(time.Hour)})
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if series == nil {
		t.Fatal("expected a series carrying the session metadata even without turns")
	}
	if series.current.Turns != 0 {
		t.Fatalf("turns = %d, want 0 when only a null-info pulse is present", series.current.Turns)
	}
}

func TestDiscoverCodexSessions_ReturnsEmptyForMissingDir(t *testing.T) {
	paths, err := discoverCodexSessions(filepath.Join(t.TempDir(), "does-not-exist"), time.Time{}, time.Time{})
	if err != nil {
		t.Fatalf("expected no error for missing dir, got %v", err)
	}
	if len(paths) != 0 {
		t.Fatalf("expected empty result, got %v", paths)
	}
}

func TestQueryHistory_IncludesCodexWorkspacesWithoutClaudeTranscripts(t *testing.T) {
	// Stage a fake HOME whose ~/.ax/codex/<name>-<hash>/sessions layout
	// matches what agent.CodexHomePath produces for a workspace.
	fakeHome := t.TempDir()
	t.Setenv("HOME", fakeHome)

	workspaceName := "worker"
	workspaceDir := filepath.Join(fakeHome, "proj")
	if err := os.MkdirAll(workspaceDir, 0o755); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}

	// Recreate agent.CodexHomePath's naming: workspace + "-" + sha1(dir)[0:12].
	codexHome := mustCodexHomePath(t, workspaceName, workspaceDir)
	sessionsDir := filepath.Join(codexHome, "sessions", "2026", "04", "14")
	writeCodexSession(t, sessionsDir, codexSessionFixture)

	since, _ := time.Parse(time.RFC3339, "2026-04-14T00:00:00Z")
	until, _ := time.Parse(time.RFC3339, "2026-04-15T00:00:00Z")
	resp, err := QueryHistory([]WorkspaceBinding{{Name: workspaceName, Dir: workspaceDir}}, HistoryQuery{
		Since:      since,
		Until:      until,
		BucketSize: 5 * time.Minute,
	})
	if err != nil {
		t.Fatalf("QueryHistory: %v", err)
	}
	if len(resp.Workspaces) != 1 {
		t.Fatalf("got %d workspaces, want 1", len(resp.Workspaces))
	}
	ws := resp.Workspaces[0]
	if !ws.Available {
		t.Fatalf("workspace reported unavailable (reason=%q); expected Codex sessions to surface", ws.UnavailableReason)
	}
	if len(ws.Agents) != 1 || ws.Agents[0].Agent != codexAgentName {
		t.Fatalf("expected single codex agent, got %+v", ws.Agents)
	}
	if ws.Agents[0].CurrentSnapshot.Turns != 2 {
		t.Fatalf("turns = %d, want 2", ws.Agents[0].CurrentSnapshot.Turns)
	}
}

func mustCodexHomePath(t *testing.T, workspace, dir string) string {
	t.Helper()
	path, err := agent.CodexHomePath(workspace, dir)
	if err != nil {
		t.Fatalf("codex home path: %v", err)
	}
	if err := os.MkdirAll(path, 0o755); err != nil {
		t.Fatalf("mkdir codex home: %v", err)
	}
	return path
}

func TestDiscoverCodexSessions_FiltersByModTime(t *testing.T) {
	root := t.TempDir()
	oldDir := filepath.Join(root, "2026", "01", "01")
	newDir := filepath.Join(root, "2026", "04", "14")
	oldPath := writeCodexSession(t, oldDir, "{}\n")
	newPath := writeCodexSession(t, newDir, "{}\n")

	// Age the old file artificially.
	oldStamp := time.Now().Add(-48 * time.Hour)
	if err := os.Chtimes(oldPath, oldStamp, oldStamp); err != nil {
		t.Fatalf("chtimes old: %v", err)
	}

	since := time.Now().Add(-24 * time.Hour)
	paths, err := discoverCodexSessions(root, since, time.Time{})
	if err != nil {
		t.Fatalf("discover: %v", err)
	}
	if len(paths) != 1 {
		t.Fatalf("expected 1 recent session, got %d: %v", len(paths), paths)
	}
	if paths[0] != newPath {
		t.Fatalf("got %s, want %s", paths[0], newPath)
	}
}
