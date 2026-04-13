package daemon

import (
	"encoding/json"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/ashon/ax/internal/types"
	"github.com/google/uuid"
)

type MessageQueue struct {
	mu       sync.Mutex
	messages map[string][]types.Message // workspace -> pending messages
	filePath string
}

func NewMessageQueue() *MessageQueue {
	return &MessageQueue{
		messages: make(map[string][]types.Message),
	}
}

func NewPersistentMessageQueue(stateDir string) *MessageQueue {
	return &MessageQueue{
		messages: make(map[string][]types.Message),
		filePath: filepath.Join(stateDir, "queue.json"),
	}
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
		return
	}
	_ = os.WriteFile(q.filePath, data, 0o644)
}
