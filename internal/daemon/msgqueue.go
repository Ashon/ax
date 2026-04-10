package daemon

import (
	"sync"
	"time"

	"github.com/ashon/amux/internal/types"
	"github.com/google/uuid"
)

type MessageQueue struct {
	mu       sync.Mutex
	messages map[string][]types.Message // workspace -> pending messages
}

func NewMessageQueue() *MessageQueue {
	return &MessageQueue{
		messages: make(map[string][]types.Message),
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
	return result
}

func (q *MessageQueue) PendingCount(workspace string) int {
	q.mu.Lock()
	defer q.mu.Unlock()
	return len(q.messages[workspace])
}
