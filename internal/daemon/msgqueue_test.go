package daemon

import (
	"bytes"
	"encoding/json"
	"log"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestMessageQueueDropsOldestWhenCapExceeded(t *testing.T) {
	q := NewMessageQueue()
	q.SetMaxSize(3)
	var buf bytes.Buffer
	q.SetLogger(log.New(&buf, "", 0))

	for i := 0; i < 5; i++ {
		q.Enqueue("orchestrator", "worker", "msg")
	}

	pending := q.Pending("worker")
	if len(pending) != 3 {
		t.Fatalf("expected queue size capped at 3, got %d", len(pending))
	}

	if !strings.Contains(buf.String(), "queue cap exceeded") {
		t.Fatalf("expected drop log line, got %q", buf.String())
	}
}

func TestMessageQueueZeroCapDisablesLimit(t *testing.T) {
	q := NewMessageQueue()
	q.SetMaxSize(0)

	for i := 0; i < 50; i++ {
		q.Enqueue("orchestrator", "worker", "msg")
	}

	if got := len(q.Pending("worker")); got != 50 {
		t.Fatalf("expected unbounded behavior, got %d", got)
	}
}

func TestPersistentMessageQueueWritesAtomicallyAndReloads(t *testing.T) {
	dir := t.TempDir()
	q := newPersistentMessageQueue(dir, time.Hour)
	defer q.Close()
	q.SetMaxSize(0)

	q.Enqueue("orchestrator", "worker", "hello")
	q.Enqueue("orchestrator", "worker", "world")
	if err := q.Flush(); err != nil {
		t.Fatalf("flush queue: %v", err)
	}

	queuePath := filepath.Join(dir, "queue.json")
	data, err := os.ReadFile(queuePath)
	if err != nil {
		t.Fatalf("read queue file: %v", err)
	}

	// Confirm the persisted file is well-formed JSON (no half-written state).
	var parsed map[string][]map[string]any
	if err := json.Unmarshal(data, &parsed); err != nil {
		t.Fatalf("queue file is not valid JSON: %v", err)
	}
	if len(parsed["worker"]) != 2 {
		t.Fatalf("expected 2 worker entries on disk, got %d", len(parsed["worker"]))
	}

	// No leftover temp files in the state dir.
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("readdir: %v", err)
	}
	for _, e := range entries {
		if strings.HasPrefix(e.Name(), ".queue.json.tmp-") {
			t.Fatalf("temp file left behind: %s", e.Name())
		}
	}

	// A fresh queue rooted at the same dir must rehydrate the same state.
	q2 := newPersistentMessageQueue(dir, time.Hour)
	defer q2.Close()
	if err := q2.Load(); err != nil {
		t.Fatalf("reload queue: %v", err)
	}
	if got := q2.PendingCount("worker"); got != 2 {
		t.Fatalf("expected 2 messages after reload, got %d", got)
	}
}

func TestPersistentMessageQueueBatchesEnqueueUntilFlush(t *testing.T) {
	dir := t.TempDir()
	q := newPersistentMessageQueue(dir, time.Hour)
	defer q.Close()

	q.Enqueue("orchestrator", "worker", "hello")
	q.Enqueue("orchestrator", "worker", "world")

	queuePath := filepath.Join(dir, "queue.json")
	if _, err := os.Stat(queuePath); !os.IsNotExist(err) {
		t.Fatalf("expected no persisted queue file before flush, got err=%v", err)
	}

	if err := q.Flush(); err != nil {
		t.Fatalf("flush queue: %v", err)
	}
	if _, err := os.Stat(queuePath); err != nil {
		t.Fatalf("expected queue file after flush: %v", err)
	}
}
