package daemon

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestRefreshTaskSnapshotsWritesWatchSnapshotWithoutRewritingDurableState(t *testing.T) {
	stateDir := t.TempDir()
	socketPath := filepath.Join(stateDir, "daemon.sock")
	d := New(socketPath)

	if _, err := d.taskStore.Create("snapshot task", "desc", "worker", "orch", "", "", "", 0); err != nil {
		t.Fatalf("create task: %v", err)
	}

	statePath := filepath.Join(stateDir, taskStateFileName)
	beforeData, err := os.ReadFile(statePath)
	if err != nil {
		t.Fatalf("read durable task state: %v", err)
	}
	beforeInfo, err := os.Stat(statePath)
	if err != nil {
		t.Fatalf("stat durable task state: %v", err)
	}

	time.Sleep(20 * time.Millisecond)

	d.refreshTaskSnapshots()
	if err := d.taskSnapshots.Flush(d.buildTaskSnapshot); err != nil {
		t.Fatalf("flush task snapshot: %v", err)
	}

	afterData, err := os.ReadFile(statePath)
	if err != nil {
		t.Fatalf("read durable task state after snapshot flush: %v", err)
	}
	afterInfo, err := os.Stat(statePath)
	if err != nil {
		t.Fatalf("stat durable task state after snapshot flush: %v", err)
	}
	if !bytes.Equal(beforeData, afterData) {
		t.Fatalf("expected durable task state to remain unchanged\nbefore=%s\nafter=%s", beforeData, afterData)
	}
	if !afterInfo.ModTime().Equal(beforeInfo.ModTime()) {
		t.Fatalf("expected durable task state mtime to remain unchanged, got %s -> %s", beforeInfo.ModTime(), afterInfo.ModTime())
	}

	snapshotData, err := os.ReadFile(TasksFilePath(socketPath))
	if err != nil {
		t.Fatalf("read materialized task snapshot: %v", err)
	}
	if !bytes.Contains(snapshotData, []byte("snapshot task")) {
		t.Fatalf("expected materialized task snapshot to contain created task, got %s", snapshotData)
	}
}
