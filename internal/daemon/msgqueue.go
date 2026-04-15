package daemon

import (
	"encoding/json"
	"log"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/ashon/ax/internal/types"
	"github.com/google/uuid"
)

// DefaultMaxQueuePerWorkspace caps how many pending messages a single
// workspace inbox may hold. When the cap is exceeded the oldest message is
// dropped so a perpetually offline (or crash-looping) workspace cannot
// exhaust daemon memory and disk.
const DefaultMaxQueuePerWorkspace = 1000
const defaultQueueFlushInterval = 100 * time.Millisecond

type MessageQueue struct {
	mu            sync.Mutex
	persistMu     sync.Mutex
	closeOnce     sync.Once
	messages      map[string][]types.Message // workspace -> pending messages
	filePath      string
	maxSize       int
	logger        *log.Logger
	flushInterval time.Duration
	stopCh        chan struct{}
	doneCh        chan struct{}
	dirty         bool
}

func NewMessageQueue() *MessageQueue {
	return &MessageQueue{
		messages: make(map[string][]types.Message),
		maxSize:  DefaultMaxQueuePerWorkspace,
	}
}

func NewPersistentMessageQueue(stateDir string) *MessageQueue {
	return newPersistentMessageQueue(stateDir, defaultQueueFlushInterval)
}

func newPersistentMessageQueue(stateDir string, flushInterval time.Duration) *MessageQueue {
	q := &MessageQueue{
		messages: make(map[string][]types.Message),
		filePath: filepath.Join(stateDir, "queue.json"),
		maxSize:  DefaultMaxQueuePerWorkspace,
	}
	if flushInterval <= 0 {
		flushInterval = defaultQueueFlushInterval
	}
	q.flushInterval = flushInterval
	q.startFlusher()
	return q
}

func (q *MessageQueue) startFlusher() {
	if q.filePath == "" {
		return
	}
	q.stopCh = make(chan struct{})
	q.doneCh = make(chan struct{})
	go func() {
		ticker := time.NewTicker(q.flushInterval)
		defer ticker.Stop()
		defer close(q.doneCh)
		for {
			select {
			case <-ticker.C:
				_ = q.Flush()
			case <-q.stopCh:
				_ = q.Flush()
				return
			}
		}
	}()
}

func (q *MessageQueue) markDirtyLocked() {
	if q.filePath == "" {
		return
	}
	q.dirty = true
}

func copyPendingMessages(src map[string][]types.Message) map[string][]types.Message {
	clone := make(map[string][]types.Message, len(src))
	for workspace, messages := range src {
		if messages == nil {
			clone[workspace] = nil
			continue
		}
		copied := make([]types.Message, len(messages))
		copy(copied, messages)
		clone[workspace] = copied
	}
	return clone
}

func (q *MessageQueue) snapshotDirty() (map[string][]types.Message, bool) {
	q.mu.Lock()
	defer q.mu.Unlock()
	if q.filePath == "" || !q.dirty {
		return nil, false
	}
	snapshot := copyPendingMessages(q.messages)
	q.dirty = false
	return snapshot, true
}

func (q *MessageQueue) persistSnapshot(snapshot map[string][]types.Message) error {
	data, err := json.Marshal(snapshot)
	if err != nil {
		return err
	}
	return writeFileAtomic(q.filePath, data, 0o600)
}

func (q *MessageQueue) Flush() error {
	if q.filePath == "" {
		return nil
	}

	q.persistMu.Lock()
	defer q.persistMu.Unlock()

	snapshot, ok := q.snapshotDirty()
	if !ok {
		return nil
	}
	if err := q.persistSnapshot(snapshot); err != nil {
		q.mu.Lock()
		q.dirty = true
		q.mu.Unlock()
		if q.logger != nil {
			q.logger.Printf("persist queue: %v", err)
		}
		return err
	}
	return nil
}

func (q *MessageQueue) Close() {
	if q.stopCh == nil {
		return
	}
	q.closeOnce.Do(func() {
		close(q.stopCh)
		<-q.doneCh
	})
}

// SetMaxSize overrides the per-workspace pending message cap. A value <= 0
// disables the cap. Intended for tests.
func (q *MessageQueue) SetMaxSize(n int) {
	q.mu.Lock()
	defer q.mu.Unlock()
	q.maxSize = n
}

// SetLogger attaches a logger so the queue can announce dropped messages
// when a workspace inbox exceeds its cap.
func (q *MessageQueue) SetLogger(l *log.Logger) {
	q.mu.Lock()
	defer q.mu.Unlock()
	q.logger = l
}

func (q *MessageQueue) Enqueue(from, to, content string) types.Message {
	return q.EnqueueMessage(types.Message{
		From:    from,
		To:      to,
		Content: content,
	})
}

func (q *MessageQueue) EnqueueMessage(msg types.Message) types.Message {
	q.mu.Lock()
	defer q.mu.Unlock()
	if msg.ID == "" {
		msg.ID = uuid.New().String()
	}
	if msg.CreatedAt.IsZero() {
		msg.CreatedAt = time.Now()
	}
	q.messages[msg.To] = append(q.messages[msg.To], msg)
	if q.maxSize > 0 && len(q.messages[msg.To]) > q.maxSize {
		dropped := len(q.messages[msg.To]) - q.maxSize
		// Drop oldest entries; new (most recent) messages win.
		q.messages[msg.To] = q.messages[msg.To][dropped:]
		if q.logger != nil {
			q.logger.Printf("queue cap exceeded for %q, dropped %d oldest message(s)", msg.To, dropped)
		}
	}
	q.markDirtyLocked()
	return msg
}

func (q *MessageQueue) RemoveTaskMessages(workspace, taskID string) int {
	q.mu.Lock()
	defer q.mu.Unlock()

	if strings.TrimSpace(taskID) == "" {
		return 0
	}
	pending := q.messages[workspace]
	if len(pending) == 0 {
		return 0
	}

	remaining := pending[:0]
	removed := 0
	for _, msg := range pending {
		if messageTaskID(msg) == taskID {
			removed++
			continue
		}
		remaining = append(remaining, msg)
	}
	if removed == 0 {
		return 0
	}
	q.messages[workspace] = remaining
	q.markDirtyLocked()
	return removed
}

func (q *MessageQueue) Dequeue(workspace string, limit int, from string) []types.Message {
	return q.DequeueIf(workspace, limit, from, nil)
}

// DequeueIf removes and returns up to limit pending messages for workspace that
// match the optional sender filter and satisfy allow when it is provided.
func (q *MessageQueue) DequeueIf(workspace string, limit int, from string, allow func(types.Message) bool) []types.Message {
	q.mu.Lock()
	defer q.mu.Unlock()

	pending := q.messages[workspace]
	if len(pending) == 0 {
		return nil
	}

	var result []types.Message
	var remaining []types.Message

	for _, msg := range pending {
		if from != "" && msg.From != from {
			remaining = append(remaining, msg)
			continue
		}
		if allow != nil && !allow(msg) {
			remaining = append(remaining, msg)
			continue
		}
		if limit > 0 && len(result) >= limit {
			remaining = append(remaining, msg)
			continue
		}
		result = append(result, msg)
	}

	if len(result) == 0 {
		return nil
	}

	q.messages[workspace] = remaining
	q.markDirtyLocked()
	return result
}

func (q *MessageQueue) PendingCountIf(workspace string, allow func(types.Message) bool) int {
	q.mu.Lock()
	defer q.mu.Unlock()

	count := 0
	for _, msg := range q.messages[workspace] {
		if allow != nil && !allow(msg) {
			continue
		}
		count++
	}
	return count
}

func (q *MessageQueue) PendingCount(workspace string) int {
	return q.PendingCountIf(workspace, nil)
}

func (q *MessageQueue) Pending(workspace string) []types.Message {
	q.mu.Lock()
	defer q.mu.Unlock()

	pending := q.messages[workspace]
	result := make([]types.Message, len(pending))
	copy(result, pending)
	return result
}

func (q *MessageQueue) HasTaskMessage(workspace, taskID string) bool {
	q.mu.Lock()
	defer q.mu.Unlock()

	for _, msg := range q.messages[workspace] {
		if messageTaskID(msg) == strings.TrimSpace(taskID) {
			return true
		}
	}
	return false
}

func (q *MessageQueue) Load() error {
	q.mu.Lock()
	defer q.mu.Unlock()

	if q.filePath == "" {
		return nil
	}
	data, err := os.ReadFile(q.filePath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	if len(data) == 0 {
		q.messages = make(map[string][]types.Message)
		return nil
	}
	var messages map[string][]types.Message
	if err := json.Unmarshal(data, &messages); err != nil {
		return err
	}
	if messages == nil {
		messages = make(map[string][]types.Message)
	}
	q.messages = messages
	q.dirty = false
	return nil
}

func messageTaskID(msg types.Message) string {
	if strings.TrimSpace(msg.TaskID) != "" {
		return strings.TrimSpace(msg.TaskID)
	}
	return extractTaskID(msg.Content)
}
