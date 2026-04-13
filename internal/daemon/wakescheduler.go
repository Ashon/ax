package daemon

import (
	"context"
	"fmt"
	"log"
	"sync"
	"time"

	"github.com/ashon/ax/internal/tmux"
)

// WakeScheduler retries tmux wake attempts for workspaces that have unread
// messages. It uses exponential backoff and checks whether the target agent
// is idle before sending keys, to avoid interfering with a busy agent.
type WakeScheduler struct {
	mu      sync.Mutex
	pending map[string]*pendingWake
	queue   *MessageQueue
	logger  *log.Logger
	notify  chan struct{} // signals the run loop to check immediately
}

type pendingWake struct {
	Workspace string
	Sender    string
	Attempts  int
	NextRetry time.Time
}

const (
	wakeCheckInterval = 3 * time.Second
	wakeMaxAttempts   = 10
)

// backoff returns the delay before the next retry attempt.
func wakeBackoff(attempt int) time.Duration {
	delays := []time.Duration{
		5 * time.Second,
		10 * time.Second,
		20 * time.Second,
		40 * time.Second,
		60 * time.Second,
	}
	if attempt >= len(delays) {
		return delays[len(delays)-1]
	}
	return delays[attempt]
}

func NewWakeScheduler(queue *MessageQueue, logger *log.Logger) *WakeScheduler {
	return &WakeScheduler{
		pending: make(map[string]*pendingWake),
		queue:   queue,
		logger:  logger,
		notify:  make(chan struct{}, 1),
	}
}

// Schedule registers a pending wake for the target workspace.
// If a wake is already pending for the workspace, it is reset.
func (s *WakeScheduler) Schedule(workspace, sender string) {
	s.mu.Lock()
	defer s.mu.Unlock()

	s.pending[workspace] = &pendingWake{
		Workspace: workspace,
		Sender:    sender,
		Attempts:  0,
		NextRetry: time.Now().Add(5 * time.Second), // first retry after 5s
	}

	// Signal the run loop
	select {
	case s.notify <- struct{}{}:
	default:
	}
}

// Cancel removes a pending wake for a workspace (e.g., when it reads messages).
func (s *WakeScheduler) Cancel(workspace string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.pending, workspace)
}

// Run starts the scheduler loop. It blocks until ctx is cancelled.
func (s *WakeScheduler) Run(ctx context.Context) {
	ticker := time.NewTicker(wakeCheckInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			s.process()
		case <-s.notify:
			// Brief delay to let the immediate wake attempt (from tools.go) go first
			time.Sleep(500 * time.Millisecond)
			s.process()
		}
	}
}

func (s *WakeScheduler) process() {
	s.mu.Lock()
	// Collect entries to process
	var ready []*pendingWake
	now := time.Now()
	for _, pw := range s.pending {
		if now.After(pw.NextRetry) || now.Equal(pw.NextRetry) {
			ready = append(ready, pw)
		}
	}
	s.mu.Unlock()

	for _, pw := range ready {
		// Check if messages are still pending
		if s.queue.PendingCount(pw.Workspace) == 0 {
			s.Cancel(pw.Workspace)
			continue
		}

		// Check if session exists
		if !tmux.SessionExists(pw.Workspace) {
			s.Cancel(pw.Workspace)
			continue
		}

		// Check if agent is idle before attempting wake
		if !tmux.IsIdle(pw.Workspace) {
			// Not idle — reschedule without incrementing attempts
			s.mu.Lock()
			if entry, ok := s.pending[pw.Workspace]; ok {
				entry.NextRetry = time.Now().Add(wakeCheckInterval)
			}
			s.mu.Unlock()
			continue
		}

		// Agent is idle and has pending messages — attempt wake
		prompt := fmt.Sprintf(
			"read_messages MCP 도구로 수신 메시지를 확인하고 요청된 작업을 수행해 줘. 결과는 send_message(to=\"%s\")로 보내줘.",
			pw.Sender,
		)
		err := tmux.WakeWorkspace(pw.Workspace, prompt)

		s.mu.Lock()
		if entry, ok := s.pending[pw.Workspace]; ok {
			entry.Attempts++
			if err != nil || entry.Attempts >= wakeMaxAttempts {
				delete(s.pending, pw.Workspace)
				if err != nil {
					s.logger.Printf("wake %q failed (attempt %d): %v", pw.Workspace, entry.Attempts, err)
				} else {
					s.logger.Printf("wake %q max attempts reached (%d)", pw.Workspace, entry.Attempts)
				}
			} else {
				entry.NextRetry = time.Now().Add(wakeBackoff(entry.Attempts))
				s.logger.Printf("wake %q attempt %d, next retry in %v", pw.Workspace, entry.Attempts, wakeBackoff(entry.Attempts))
			}
		}
		s.mu.Unlock()
	}
}
