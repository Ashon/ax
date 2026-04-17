package daemon

import (
	"context"
	"log"
	"sync"
	"time"

	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/tmux"
)

var (
	wakeSchedulerSessionExists = tmux.SessionExists
	wakeSchedulerSessionIdle   = tmux.IsIdle
	wakeSchedulerWakeWorkspace = tmux.WakeWorkspace
)

// WakeScheduler retries tmux wake attempts for workspaces that have unread
// messages. It uses exponential backoff and checks whether the target agent
// is idle before sending keys, to avoid interfering with a busy agent.
type WakeScheduler struct {
	mu                       sync.Mutex
	pending                  map[string]*pendingWake
	queue                    *MessageQueue
	logger                   *log.Logger
	notify                   chan struct{} // signals the run loop to check immediately
	refill                   func(workspace string) int
	ensureSession            func(workspace, sender string) bool
	retryAfterSuccessfulWake func(workspace string) bool
}

type pendingWake struct {
	Workspace string
	Sender    string
	Attempts  int
	NextRetry time.Time
}

// WakeState is a snapshot of the retry metadata for a workspace with a pending
// wake request.
type WakeState struct {
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

// NewWakeScheduler creates a scheduler that tracks unread-message wake retries
// for active workspaces.
func NewWakeScheduler(queue *MessageQueue, logger *log.Logger) *WakeScheduler {
	return &WakeScheduler{
		pending: make(map[string]*pendingWake),
		queue:   queue,
		logger:  logger,
		notify:  make(chan struct{}, 1),
	}
}

// SetQueueRefiller installs an optional callback that can rehydrate runnable
// task messages when a wake retry finds an empty inbox for the target
// workspace. The callback should return the number of messages it re-enqueued.
func (s *WakeScheduler) SetQueueRefiller(refill func(workspace string) int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.refill = refill
}

// SetMissingSessionEnsurer installs an optional callback that can recreate a
// managed target when wake processing finds pending work but no live tmux
// session for the workspace.
func (s *WakeScheduler) SetMissingSessionEnsurer(fn func(workspace, sender string) bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.ensureSession = fn
}

// SetRetryAfterSuccessfulWake installs an optional policy hook that decides
// whether a workspace should remain on the retry schedule after a successful
// tmux wake. Returning false removes the workspace from the retry queue.
func (s *WakeScheduler) SetRetryAfterSuccessfulWake(fn func(workspace string) bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.retryAfterSuccessfulWake = fn
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

// State returns a copy of the current retry state for a workspace, if one is
// pending.
func (s *WakeScheduler) State(workspace string) (WakeState, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()

	entry, ok := s.pending[workspace]
	if !ok {
		return WakeState{}, false
	}
	return WakeState{
		Workspace: entry.Workspace,
		Sender:    entry.Sender,
		Attempts:  entry.Attempts,
		NextRetry: entry.NextRetry,
	}, true
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
			if s.refill != nil {
				rehydrated := s.refill(pw.Workspace)
				if rehydrated > 0 && s.logger != nil {
					s.logger.Printf("wake %q rehydrated %d runnable task message(s)", pw.Workspace, rehydrated)
				}
			}
		}
		if s.queue.PendingCount(pw.Workspace) == 0 {
			s.Cancel(pw.Workspace)
			continue
		}

		if !wakeSchedulerSessionExists(pw.Workspace) {
			ensured := false
			if s.ensureSession != nil {
				ensured = s.ensureSession(pw.Workspace, pw.Sender)
			}
			if !ensured {
				s.Cancel(pw.Workspace)
				continue
			}

			s.mu.Lock()
			if entry, ok := s.pending[pw.Workspace]; ok {
				entry.Attempts++
				keepRetrying := true
				if s.retryAfterSuccessfulWake != nil {
					keepRetrying = s.retryAfterSuccessfulWake(pw.Workspace)
				}
				if entry.Attempts >= wakeMaxAttempts || !keepRetrying {
					delete(s.pending, pw.Workspace)
					if s.logger != nil {
						if !keepRetrying {
							s.logger.Printf("wake %q ensured a session and retry policy cleared further nudges", pw.Workspace)
						} else {
							s.logger.Printf("wake %q max attempts reached (%d)", pw.Workspace, entry.Attempts)
						}
					}
				} else {
					entry.NextRetry = time.Now().Add(wakeBackoff(entry.Attempts))
					if s.logger != nil {
						s.logger.Printf("wake %q ensured a session, next retry in %v", pw.Workspace, wakeBackoff(entry.Attempts))
					}
				}
			}
			s.mu.Unlock()
			continue
		}

		// Check if agent is idle before attempting wake
		if !wakeSchedulerSessionIdle(pw.Workspace) {
			// Not idle — reschedule without incrementing attempts
			s.mu.Lock()
			if entry, ok := s.pending[pw.Workspace]; ok {
				entry.NextRetry = time.Now().Add(wakeCheckInterval)
			}
			s.mu.Unlock()
			continue
		}

		// Agent is idle and has pending messages — attempt wake
		err := wakeSchedulerWakeWorkspace(pw.Workspace, daemonutil.WakePrompt(pw.Sender, false))

		s.mu.Lock()
		if entry, ok := s.pending[pw.Workspace]; ok {
			entry.Attempts++
			keepRetrying := true
			if err == nil && s.retryAfterSuccessfulWake != nil {
				keepRetrying = s.retryAfterSuccessfulWake(pw.Workspace)
			}
			if err != nil || entry.Attempts >= wakeMaxAttempts || !keepRetrying {
				delete(s.pending, pw.Workspace)
				if s.logger != nil {
					if err != nil {
						s.logger.Printf("wake %q failed (attempt %d): %v", pw.Workspace, entry.Attempts, err)
					} else if !keepRetrying {
						s.logger.Printf("wake %q succeeded and retry policy cleared further nudges", pw.Workspace)
					} else {
						s.logger.Printf("wake %q max attempts reached (%d)", pw.Workspace, entry.Attempts)
					}
				}
			} else {
				entry.NextRetry = time.Now().Add(wakeBackoff(entry.Attempts))
				if s.logger != nil {
					s.logger.Printf("wake %q attempt %d, next retry in %v", pw.Workspace, entry.Attempts, wakeBackoff(entry.Attempts))
				}
			}
		}
		s.mu.Unlock()
	}
}
