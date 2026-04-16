package daemon

import (
	"context"
	"time"

	"github.com/ashon/ax/internal/tmux"
)

var (
	idleSleepSessionExists = tmux.SessionExists
	idleSleepSessionIdle   = tmux.IsIdle
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
	d.sessionManager().stopIdle(now)
}

func (d *Daemon) shouldSleepWorkspace(registered RegisteredWorkspace, now time.Time) bool {
	return d.sessionManager().shouldSleep(registered, now)
}

func (d *Daemon) hasOpenAssignedTasks(assignee string) bool {
	return d.sessionManager().hasOpenAssignedTasks(assignee)
}
