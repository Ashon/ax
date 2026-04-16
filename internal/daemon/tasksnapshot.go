package daemon

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"log"
	"sort"
	"sync"
	"time"

	"github.com/ashon/ax/internal/types"
)

const defaultTaskSnapshotFlushInterval = 200 * time.Millisecond

// taskSnapshotWriter materializes enriched task snapshots for UI readers
// without coupling general daemon request handling to synchronous file writes.
type taskSnapshotWriter struct {
	path          string
	logger        *log.Logger
	flushInterval time.Duration

	persistMu sync.Mutex

	mu        sync.Mutex
	version   uint64
	flushed   uint64
	lastHash  [sha256.Size]byte
	hasDigest bool
}

func newTaskSnapshotWriter(path string, flushInterval time.Duration, logger *log.Logger) *taskSnapshotWriter {
	if flushInterval <= 0 {
		flushInterval = defaultTaskSnapshotFlushInterval
	}
	return &taskSnapshotWriter{
		path:          path,
		logger:        logger,
		flushInterval: flushInterval,
	}
}

func (w *taskSnapshotWriter) MarkDirty() {
	if w == nil {
		return
	}
	w.mu.Lock()
	w.version++
	w.mu.Unlock()
}

func (w *taskSnapshotWriter) Run(ctx context.Context, produce func() []types.Task) {
	if w == nil {
		return
	}
	ticker := time.NewTicker(w.flushInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			_ = w.Flush(produce)
			return
		case <-ticker.C:
			_ = w.Flush(produce)
		}
	}
}

func (w *taskSnapshotWriter) Flush(produce func() []types.Task) error {
	if w == nil || w.path == "" || produce == nil {
		return nil
	}

	w.persistMu.Lock()
	defer w.persistMu.Unlock()

	w.mu.Lock()
	if w.version == w.flushed {
		w.mu.Unlock()
		return nil
	}
	targetVersion := w.version
	prevHash := w.lastHash
	hadDigest := w.hasDigest
	w.mu.Unlock()

	tasks := produce()
	if tasks == nil {
		tasks = []types.Task{}
	}
	sort.Slice(tasks, func(i, j int) bool {
		return tasks[i].ID < tasks[j].ID
	})

	data, err := json.Marshal(tasks)
	if err != nil {
		return err
	}
	hash := sha256.Sum256(data)
	if hadDigest && hash == prevHash {
		w.mu.Lock()
		if w.flushed < targetVersion {
			w.flushed = targetVersion
		}
		w.mu.Unlock()
		return nil
	}

	if err := writeFileAtomic(w.path, data, 0o600); err != nil {
		if w.logger != nil {
			w.logger.Printf("persist task snapshot: %v", err)
		}
		return err
	}

	w.mu.Lock()
	if w.flushed < targetVersion {
		w.flushed = targetVersion
	}
	w.lastHash = hash
	w.hasDigest = true
	w.mu.Unlock()
	return nil
}
