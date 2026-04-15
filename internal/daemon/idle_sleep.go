package daemon

import (
	"context"
	"strings"
	"time"

	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/workspace"
)

var (
	idleSleepSessionExists = tmux.SessionExists
	idleSleepSessionIdle   = tmux.IsIdle
	idleSleepStopTarget    = workspace.StopNamedTarget
)

const idleSleepCheckInterval = time.Minute

func (d *Daemon) runIdleSleepLoop(ctx context.Context) {
	ticker := time.NewTicker(idleSleepCheckInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			d.processIdleSleep(time.Now())
		}
	}
}

func (d *Daemon) processIdleSleep(now time.Time) {
	for _, registered := range d.registry.Snapshot() {
		if !d.shouldSleepWorkspace(registered, now) {
			continue
		}
		if _, err := idleSleepStopTarget(d.socketPath, registered.ConfigPath, registered.Info.Name); err != nil {
			if d.logger != nil {
				d.logger.Printf("idle sleep skip for %q: %v", registered.Info.Name, err)
			}
			d.registry.Touch(registered.Info.Name)
			continue
		}
		if d.logger != nil {
			d.logger.Printf("idle sleep: stopped %q after %s without queued work", registered.Info.Name, now.Sub(registered.LastActiveAt).Round(time.Second))
		}
		// If the client disconnect lags slightly behind the tmux stop, avoid
		// hammering repeated stop attempts against the same stale registration.
		d.registry.Touch(registered.Info.Name)
	}
}

func (d *Daemon) shouldSleepWorkspace(registered RegisteredWorkspace, now time.Time) bool {
	name := strings.TrimSpace(registered.Info.Name)
	if name == "" || name == "orchestrator" {
		return false
	}
	if strings.TrimSpace(registered.ConfigPath) == "" {
		return false
	}
	if registered.IdleTimeout <= 0 {
		return false
	}
	if registered.LastActiveAt.IsZero() || now.Sub(registered.LastActiveAt) < registered.IdleTimeout {
		return false
	}
	if !idleSleepSessionExists(name) || !idleSleepSessionIdle(name) {
		return false
	}
	if d.queue.PendingCount(name) > 0 {
		return false
	}
	if d.wakeScheduler != nil {
		if _, ok := d.wakeScheduler.State(name); ok {
			return false
		}
	}
	return !d.hasOpenAssignedTasks(name)
}

func (d *Daemon) hasOpenAssignedTasks(assignee string) bool {
	tasks := d.taskStore.List(assignee, "", nil)
	for _, task := range tasks {
		switch task.Status {
		case types.TaskPending, types.TaskInProgress, types.TaskBlocked:
			return true
		}
	}
	return false
}
