package daemon

import (
	"encoding/json"
	"log"
	"os"
	"path/filepath"
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

type MessageQueue struct {
	mu       sync.Mutex
	messages map[string][]types.Message // workspace -> pending messages
	filePath string
	maxSize  int
	logger   *log.Logger
}

func NewMessageQueue() *MessageQueue {
	return &MessageQueue{
		messages: make(map[string][]types.Message),
		maxSize:  DefaultMaxQueuePerWorkspace,
	}
}

func NewPersistentMessageQueue(stateDir string) *MessageQueue {
	return &MessageQueue{
		messages: make(map[string][]types.Message),
		filePath: filepath.Join(stateDir, "queue.json"),
		maxSize:  DefaultMaxQueuePerWorkspace,
	}
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
	q.mu.Lock()
	defer q.mu.Unlock()
	msg := types.Message{
		ID:        uuid.New().String(),
		From:      from,
		To:        to,
		Content:   content,
		CreatedAt: time.Now(),
	}
	q.messages[to] = append(q.messages[to], msg)
	if q.maxSize > 0 && len(q.messages[to]) > q.maxSize {
		dropped := len(q.messages[to]) - q.maxSize
		// Drop oldest entries; new (most recent) messages win.
		q.messages[to] = q.messages[to][dropped:]
		if q.logger != nil {
			q.logger.Printf("queue cap exceeded for %q, dropped %d oldest message(s)", to, dropped)
		}
	}
	q.persist()
	return msg
}

func (q *MessageQueue) Dequeue(workspace string, limit int, from string) []types.Message {
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
		if limit > 0 && len(result) >= limit {
			remaining = append(remaining, msg)
			continue
		}
		result = append(result, msg)
	}

	q.messages[workspace] = remaining
	q.persist()
	return result
}

func (q *MessageQueue) PendingCount(workspace string) int {
	q.mu.Lock()
	defer q.mu.Unlock()
	return len(q.messages[workspace])
}

func (q *MessageQueue) Pending(workspace string) []types.Message {
	q.mu.Lock()
	defer q.mu.Unlock()

	pending := q.messages[workspace]
	result := make([]types.Message, len(pending))
	copy(result, pending)
	return result
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
	return nil
}

func (q *MessageQueue) persist() {
	if q.filePath == "" {
		return
	}
	data, err := json.Marshal(q.messages)
	if err != nil {
		if q.logger != nil {
			q.logger.Printf("marshal queue: %v", err)
		}
		return
	}
	if err := writeFileAtomic(q.filePath, data, 0o600); err != nil {
		if q.logger != nil {
			q.logger.Printf("persist queue: %v", err)
		}
	}
}
