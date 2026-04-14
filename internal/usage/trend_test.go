package usage

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestQueryWorkspaceTrendBucketsUsageAcrossMatchingTranscripts(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cwd := "/tmp/ax-cli"
	projectDir := filepath.Join(home, ".claude", "projects", ProjectDirFromCwd(cwd))
	if err := os.MkdirAll(projectDir, 0o755); err != nil {
		t.Fatalf("mkdir project dir: %v", err)
	}

	now := time.Date(2026, 4, 14, 16, 0, 0, 0, time.UTC)
	lines := []string{
		`{"type":"assistant","timestamp":"2026-04-14T12:30:00Z","sessionId":"sess-1","cwd":"/tmp/ax-cli","message":{"role":"assistant","model":"claude-opus-4-6","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":25,"cache_creation_input_tokens":10}}}`,
		`{"type":"assistant","timestamp":"2026-04-14T15:10:00Z","sessionId":"sess-2","cwd":"/tmp/ax-cli","message":{"role":"assistant","model":"claude-sonnet-4-5","usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":40,"cache_creation_input_tokens":20}}}`,
	}
	if err := os.WriteFile(filepath.Join(projectDir, "trend.jsonl"), []byte(lines[0]+"\n"+lines[1]+"\n"), 0o644); err != nil {
		t.Fatalf("write transcript: %v", err)
	}

	if err := os.WriteFile(filepath.Join(projectDir, "other.jsonl"), []byte(`{"type":"assistant","timestamp":"2026-04-14T15:30:00Z","sessionId":"sess-x","cwd":"/tmp/other","message":{"role":"assistant","model":"claude-haiku","usage":{"input_tokens":999,"output_tokens":999,"cache_read_input_tokens":999,"cache_creation_input_tokens":999}}}`+"\n"), 0o644); err != nil {
		t.Fatalf("write unrelated transcript: %v", err)
	}

	trend := QueryWorkspaceTrend("ax.cli", cwd, now, 6*time.Hour, 3*time.Hour)
	if !trend.Available {
		t.Fatalf("expected trend to be available, got %+v", trend)
	}
	if got, want := len(trend.Buckets), 2; got != want {
		t.Fatalf("bucket count = %d, want %d", got, want)
	}
	if trend.Total.Total() != 525 {
		t.Fatalf("total tokens = %d, want 525", trend.Total.Total())
	}
	if got := trend.Buckets[0].Totals.Total(); got != 185 {
		t.Fatalf("bucket[0] total = %d, want 185", got)
	}
	if got := trend.Buckets[1].Totals.Total(); got != 340 {
		t.Fatalf("bucket[1] total = %d, want 340", got)
	}
	if trend.LatestModel != "claude-sonnet-4-5" {
		t.Fatalf("latest model = %q", trend.LatestModel)
	}
	if trend.LastActivity == nil || !trend.LastActivity.Equal(time.Date(2026, 4, 14, 15, 10, 0, 0, time.UTC)) {
		t.Fatalf("unexpected last activity: %+v", trend.LastActivity)
	}
}
