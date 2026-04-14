package usage

import (
	"os"
	"path/filepath"
	"testing"
)

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func appendFile(t *testing.T, path, content string) {
	t.Helper()
	f, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		t.Fatalf("open append %s: %v", path, err)
	}
	defer f.Close()
	if _, err := f.WriteString(content); err != nil {
		t.Fatalf("append %s: %v", path, err)
	}
}

func TestTailer_IncrementalTailAndRotation(t *testing.T) {
	dir := t.TempDir()
	// Initial transcript with one user + one usage line.
	path1 := filepath.Join(dir, "sess-1.jsonl")
	writeFile(t, path1, lineUser+"\n"+lineOpus1+"\n")

	tl := NewTailer("/tmp/x")
	n, err := tl.tickPath(path1)
	if err != nil {
		t.Fatalf("tick1: %v", err)
	}
	if n != 1 {
		t.Errorf("ingest=%d, want 1", n)
	}
	snap := tl.Aggregator().Snapshot("ws", path1)
	if snap.CumulativeTotals.Output != 295 {
		t.Errorf("output=%d, want 295", snap.CumulativeTotals.Output)
	}
	firstOffset := tl.Offset()
	if firstOffset == 0 {
		t.Error("offset did not advance")
	}

	// Append a second usage line; tick should only parse the new bytes.
	appendFile(t, path1, lineOpus2+"\n")
	n2, err := tl.tickPath(path1)
	if err != nil {
		t.Fatalf("tick2: %v", err)
	}
	if n2 != 1 {
		t.Errorf("incremental ingest=%d, want 1", n2)
	}
	if tl.Offset() <= firstOffset {
		t.Error("offset did not advance on tick2")
	}
	snap2 := tl.Aggregator().Snapshot("ws", path1)
	// Cumulative should now include both opus lines.
	if snap2.CumulativeTotals.Output != 295+777 {
		t.Errorf("cumulative output=%d, want %d", snap2.CumulativeTotals.Output, 295+777)
	}
	if snap2.Turns != 2 {
		t.Errorf("turns=%d, want 2", snap2.Turns)
	}
	// CurrentContext should reflect lineOpus2.
	if snap2.CurrentContext.CacheRead != 182166 {
		t.Errorf("current cache_read=%d", snap2.CurrentContext.CacheRead)
	}

	// Rotate to a new session file: state must reset.
	path2 := filepath.Join(dir, "sess-2.jsonl")
	writeFile(t, path2, lineSonnet+"\n")
	n3, err := tl.tickPath(path2)
	if err != nil {
		t.Fatalf("tick3: %v", err)
	}
	if n3 != 1 {
		t.Errorf("rotation ingest=%d, want 1", n3)
	}
	snap3 := tl.Aggregator().Snapshot("ws", path2)
	if snap3.Turns != 1 {
		t.Errorf("post-rotate turns=%d, want 1", snap3.Turns)
	}
	if snap3.CurrentModel != "claude-sonnet-4-5" {
		t.Errorf("post-rotate model=%q", snap3.CurrentModel)
	}
	// Cumulative from first file must not leak across rotation.
	if snap3.CumulativeTotals.Output != 120 {
		t.Errorf("post-rotate output=%d, want 120", snap3.CumulativeTotals.Output)
	}
}

func TestTailer_PartialLineCarryover(t *testing.T) {
	// A write that ends mid-line must not be parsed until the newline arrives.
	dir := t.TempDir()
	path := filepath.Join(dir, "partial.jsonl")
	// Write lineOpus1 minus the closing brace and newline.
	partial := lineOpus1[:len(lineOpus1)-10]
	writeFile(t, path, partial)
	tl := NewTailer("/tmp/x")
	n, err := tl.tickPath(path)
	if err != nil {
		t.Fatalf("tick: %v", err)
	}
	if n != 0 {
		t.Errorf("should not ingest partial line, got %d", n)
	}
	// Complete the line.
	appendFile(t, path, lineOpus1[len(lineOpus1)-10:]+"\n")
	n2, err := tl.tickPath(path)
	if err != nil {
		t.Fatalf("tick2: %v", err)
	}
	if n2 != 1 {
		t.Errorf("after completion ingest=%d, want 1", n2)
	}
}

func TestSelectTranscript_NoProjectDir(t *testing.T) {
	_, err := selectTranscript(filepath.Join(t.TempDir(), "missing"), "/tmp/x")
	if err != ErrNoTranscript {
		t.Errorf("want ErrNoTranscript, got %v", err)
	}
}

func TestSelectTranscript_PicksNewestMatching(t *testing.T) {
	dir := t.TempDir()
	// Older file with matching cwd.
	older := filepath.Join(dir, "old.jsonl")
	writeFile(t, older, `{"type":"user","cwd":"/tmp/x","sessionId":"a"}`+"\n")
	// Newer file with mismatched cwd should be rejected.
	wrong := filepath.Join(dir, "wrong.jsonl")
	writeFile(t, wrong, `{"type":"user","cwd":"/other/path","sessionId":"b"}`+"\n")
	// Set mtime ordering: wrong newer than older.
	bumpMtime(t, wrong)
	got, err := selectTranscript(dir, "/tmp/x")
	if err != nil {
		t.Fatalf("selectTranscript: %v", err)
	}
	if got != older {
		t.Errorf("picked %s, want %s", got, older)
	}
}

// bumpMtime touches a file so its mtime is strictly greater than the
// other files in the same dir. We can't rely on sub-second mtime
// resolution on all filesystems, so we write then re-stat.
func bumpMtime(t *testing.T, path string) {
	t.Helper()
	// A tiny re-write advances mtime.
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read for bump: %v", err)
	}
	// Small sleep-free approach: use os.Chtimes to force a later mtime.
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	newer := info.ModTime().Add(1000000000) // +1s
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatalf("rewrite: %v", err)
	}
	if err := os.Chtimes(path, newer, newer); err != nil {
		t.Fatalf("chtimes: %v", err)
	}
}
