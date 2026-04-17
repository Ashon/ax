package daemon

import (
	"io"
	"log"
	"net"
	"testing"
)

// dispatchTestDaemon builds a Daemon configured with an injected dispatch
// function, wired through a fresh sessionManager. Tests can call the returned
// register() helper to hook an in-memory client connection to a workspace name.
type dispatchTestDaemon struct {
	*Daemon
	t        *testing.T
	stateDir string
	pipes    []net.Conn
}

func newDispatchTestDaemon(t *testing.T, dispatchFn dispatchRunnableFunc) *dispatchTestDaemon {
	t.Helper()
	stateDir := t.TempDir()
	d := &Daemon{
		socketPath:    "/tmp/ax.sock",
		queue:         NewMessageQueue(),
		history:       NewHistory(stateDir, 50),
		registry:      NewRegistry(),
		taskStore:     NewTaskStore(stateDir),
		wakeScheduler: NewWakeScheduler(NewMessageQueue(), nil),
		logger:        log.New(io.Discard, "", 0),
	}
	d.sessionMgr = newSessionManager(sessionManagerDeps{
		socketPath:       d.socketPath,
		registry:         d.registry,
		queue:            d.queue,
		taskStore:        d.taskStore,
		wakeScheduler:    d.wakeScheduler,
		logger:           d.logger,
		dispatchRunnable: dispatchFn,
	})
	return &dispatchTestDaemon{Daemon: d, t: t, stateDir: stateDir}
}

func (td *dispatchTestDaemon) register(name, configPath string) {
	td.t.Helper()
	client, server := net.Pipe()
	td.pipes = append(td.pipes, client, server)
	td.t.Cleanup(func() {
		client.Close()
		server.Close()
	})
	td.registry.Register(name, "", "", configPath, 0, client)
}
