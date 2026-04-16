package daemon

import (
	"fmt"
	"log"
	"strings"
	"time"

	"github.com/ashon/ax/internal/types"
)

type sessionManager struct {
	socketPath    string
	registry      *Registry
	queue         *MessageQueue
	taskStore     *TaskStore
	wakeScheduler *WakeScheduler
	logger        *log.Logger
}

func newSessionManager(socketPath string, registry *Registry, queue *MessageQueue, taskStore *TaskStore, wakeScheduler *WakeScheduler, logger *log.Logger) *sessionManager {
	return &sessionManager{
		socketPath:    socketPath,
		registry:      registry,
		queue:         queue,
		taskStore:     taskStore,
		wakeScheduler: wakeScheduler,
		logger:        logger,
	}
}

func isAlwaysOnTarget(name string) bool {
	name = strings.TrimSpace(name)
	return name == "orchestrator" || strings.HasSuffix(name, ".orchestrator")
}

func (m *sessionManager) control(configPath, targetName string, action types.LifecycleAction) (types.LifecycleTarget, error) {
	switch action {
	case types.LifecycleActionStart:
		return controlStartNamedTarget(m.socketPath, configPath, targetName)
	case types.LifecycleActionStop:
		return controlStopNamedTarget(m.socketPath, configPath, targetName)
	case types.LifecycleActionRestart:
		return controlRestartNamedTarget(m.socketPath, configPath, targetName)
	default:
		return types.LifecycleTarget{}, fmt.Errorf("invalid lifecycle action %q", action)
	}
}

func (m *sessionManager) ensureRunnable(configPath, target, sender string, fresh bool) error {
	return daemonDispatchRunnableWork(m.socketPath, configPath, target, sender, fresh)
}

func (m *sessionManager) stopIdle(now time.Time) {
	if m == nil || m.registry == nil {
		return
	}
	for _, registered := range m.registry.Snapshot() {
		if !m.shouldSleep(registered, now) {
			continue
		}
		if _, err := m.control(registered.ConfigPath, registered.Info.Name, types.LifecycleActionStop); err != nil {
			if m.logger != nil {
				m.logger.Printf("idle sleep skip for %q: %v", registered.Info.Name, err)
			}
			m.touch(registered.Info.Name)
			continue
		}
		if m.logger != nil {
			m.logger.Printf("idle sleep: stopped %q after %s without queued work", registered.Info.Name, now.Sub(registered.LastActiveAt).Round(time.Second))
		}
		m.touch(registered.Info.Name)
	}
}

func (m *sessionManager) shouldSleep(registered RegisteredWorkspace, now time.Time) bool {
	name := strings.TrimSpace(registered.Info.Name)
	if name == "" || isAlwaysOnTarget(name) {
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
	if m.queue != nil && m.queue.PendingCount(name) > 0 {
		return false
	}
	if m.wakeScheduler != nil {
		if _, ok := m.wakeScheduler.State(name); ok {
			return false
		}
	}
	return !m.hasOpenAssignedTasks(name)
}

func (m *sessionManager) hasOpenAssignedTasks(assignee string) bool {
	if m == nil || m.taskStore == nil {
		return false
	}
	tasks := m.taskStore.List(assignee, "", nil)
	for _, task := range tasks {
		switch task.Status {
		case types.TaskPending, types.TaskInProgress, types.TaskBlocked:
			return true
		}
	}
	return false
}

func (m *sessionManager) ensurePendingWakeTarget(workspace, sender string) bool {
	if m == nil || strings.TrimSpace(workspace) == "" {
		return false
	}
	if wakeSchedulerSessionExists(workspace) {
		return true
	}
	configPath, fresh := m.pendingWakeDispatchConfig(workspace)
	if configPath == "" {
		return false
	}
	if err := m.ensureRunnable(configPath, workspace, sender, fresh); err != nil {
		if m.logger != nil {
			m.logger.Printf("wake %q could not ensure runnable target: %v", workspace, err)
		}
		return false
	}
	return true
}

func (m *sessionManager) pendingWakeDispatchConfig(workspace string) (string, bool) {
	if m == nil || m.taskStore == nil {
		return "", false
	}
	runnable := m.taskStore.RunnableByAssignee(workspace, time.Now())
	for _, task := range runnable {
		configPath := strings.TrimSpace(task.DispatchConfigPath)
		if configPath == "" {
			continue
		}
		return configPath, task.StartMode == types.TaskStartFresh
	}
	return "", false
}

func (m *sessionManager) touch(name string) {
	if m == nil || m.registry == nil {
		return
	}
	m.registry.Touch(name)
}
