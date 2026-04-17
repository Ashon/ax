package daemon

import (
	"io"
	"log"
	"net"
	"testing"
	"time"

	"github.com/ashon/ax/internal/types"
)

func installIdleStopStub(d *Daemon, stop lifecycleControlFunc) {
	d.sessionMgr = newSessionManager(sessionManagerDeps{
		socketPath:    d.socketPath,
		registry:      d.registry,
		queue:         d.queue,
		taskStore:     d.taskStore,
		wakeScheduler: d.wakeScheduler,
		logger:        d.logger,
		stopTarget:    stop,
	})
}

func TestProcessIdleSleepStopsEligibleWorkspace(t *testing.T) {
	restoreIdleSleepTmuxStubs(t)

	stateDir := t.TempDir()
	d := &Daemon{
		socketPath:    "/tmp/ax.sock",
		queue:         NewMessageQueue(),
		registry:      NewRegistry(),
		taskStore:     NewTaskStore(stateDir),
		wakeScheduler: NewWakeScheduler(NewMessageQueue(), nil),
		logger:        log.New(io.Discard, "", 0),
	}

	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	entry, _ := d.registry.Register("worker", "", "", "/tmp/project/.ax/config.yaml", 15*time.Minute, clientConn)
	now := time.Now()
	entry.lastActiveAt = now.Add(-20 * time.Minute)

	idleSleepSessionExists = func(name string) bool { return name == "worker" }
	idleSleepSessionIdle = func(name string) bool { return name == "worker" }

	stopped := false
	installIdleStopStub(d, func(socketPath, configPath, target string) (types.LifecycleTarget, error) {
		stopped = true
		if socketPath != "/tmp/ax.sock" {
			t.Fatalf("unexpected socket path %q", socketPath)
		}
		if configPath != "/tmp/project/.ax/config.yaml" {
			t.Fatalf("unexpected config path %q", configPath)
		}
		if target != "worker" {
			t.Fatalf("unexpected target %q", target)
		}
		return types.LifecycleTarget{Name: target, Kind: types.LifecycleTargetWorkspace, ManagedSession: true}, nil
	})

	d.processIdleSleep(now)

	if !stopped {
		t.Fatal("expected idle workspace to be stopped")
	}
	if !entry.lastActiveAt.After(now.Add(-time.Second)) {
		t.Fatalf("expected lastActiveAt to be refreshed after stop attempt, got %s", entry.lastActiveAt)
	}
}

func TestProcessIdleSleepSkipsWorkspaceWithOpenTasks(t *testing.T) {
	restoreIdleSleepTmuxStubs(t)

	stateDir := t.TempDir()
	d := &Daemon{
		socketPath:    "/tmp/ax.sock",
		queue:         NewMessageQueue(),
		registry:      NewRegistry(),
		taskStore:     NewTaskStore(stateDir),
		wakeScheduler: NewWakeScheduler(NewMessageQueue(), nil),
		logger:        log.New(io.Discard, "", 0),
	}

	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	entry, _ := d.registry.Register("worker", "", "", "/tmp/project/.ax/config.yaml", 15*time.Minute, clientConn)
	now := time.Now()
	entry.lastActiveAt = now.Add(-20 * time.Minute)

	if _, err := d.taskStore.Create("Task still active", "", "worker", "orch", "", types.TaskStartDefault, types.TaskPriorityNormal, 0); err != nil {
		t.Fatalf("create task: %v", err)
	}

	idleSleepSessionExists = func(name string) bool { return true }
	idleSleepSessionIdle = func(name string) bool { return true }
	installIdleStopStub(d, func(socketPath, configPath, target string) (types.LifecycleTarget, error) {
		t.Fatal("idle sleep should not stop a workspace with open tasks")
		return types.LifecycleTarget{}, nil
	})

	d.processIdleSleep(now)
}

func TestProcessIdleSleepSkipsRootOrchestrator(t *testing.T) {
	restoreIdleSleepTmuxStubs(t)

	stateDir := t.TempDir()
	d := &Daemon{
		socketPath:    "/tmp/ax.sock",
		queue:         NewMessageQueue(),
		registry:      NewRegistry(),
		taskStore:     NewTaskStore(stateDir),
		wakeScheduler: NewWakeScheduler(NewMessageQueue(), nil),
		logger:        log.New(io.Discard, "", 0),
	}

	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	entry, _ := d.registry.Register("orchestrator", "", "", "/tmp/project/.ax/config.yaml", 15*time.Minute, clientConn)
	now := time.Now()
	entry.lastActiveAt = now.Add(-20 * time.Minute)

	idleSleepSessionExists = func(name string) bool { return true }
	idleSleepSessionIdle = func(name string) bool { return true }
	installIdleStopStub(d, func(socketPath, configPath, target string) (types.LifecycleTarget, error) {
		t.Fatal("root orchestrator should not be auto-slept")
		return types.LifecycleTarget{}, nil
	})

	d.processIdleSleep(now)
}

func TestProcessIdleSleepSkipsChildOrchestrator(t *testing.T) {
	restoreIdleSleepTmuxStubs(t)

	stateDir := t.TempDir()
	d := &Daemon{
		socketPath:    "/tmp/ax.sock",
		queue:         NewMessageQueue(),
		registry:      NewRegistry(),
		taskStore:     NewTaskStore(stateDir),
		wakeScheduler: NewWakeScheduler(NewMessageQueue(), nil),
		logger:        log.New(io.Discard, "", 0),
	}

	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	entry, _ := d.registry.Register("team.orchestrator", "", "", "/tmp/project/.ax/config.yaml", 15*time.Minute, clientConn)
	now := time.Now()
	entry.lastActiveAt = now.Add(-20 * time.Minute)

	idleSleepSessionExists = func(name string) bool { return true }
	idleSleepSessionIdle = func(name string) bool { return true }
	installIdleStopStub(d, func(socketPath, configPath, target string) (types.LifecycleTarget, error) {
		t.Fatal("child orchestrator should not be auto-slept")
		return types.LifecycleTarget{}, nil
	})

	d.processIdleSleep(now)
}

func restoreIdleSleepTmuxStubs(t *testing.T) {
	t.Helper()

	oldExists := idleSleepSessionExists
	oldIdle := idleSleepSessionIdle
	t.Cleanup(func() {
		idleSleepSessionExists = oldExists
		idleSleepSessionIdle = oldIdle
	})
}
