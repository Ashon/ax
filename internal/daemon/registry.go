package daemon

import (
	"net"
	"sync"
	"time"

	"github.com/ashon/ax/internal/types"
)

// outboxCapacity bounds how many envelopes can be queued in a per-connection
// outbox before Send begins to block. The limit prevents a slow or stalled
// receiver from causing unbounded memory growth on the daemon.
const outboxCapacity = 256

type connEntry struct {
	info         types.WorkspaceInfo
	configPath   string
	idleTimeout  time.Duration
	lastActiveAt time.Time
	conn         net.Conn
	outbox       chan *Envelope
	closeCh      chan struct{}
	once         sync.Once
}

func newConnEntry(info types.WorkspaceInfo, configPath string, idleTimeout time.Duration, lastActiveAt time.Time, conn net.Conn) *connEntry {
	return &connEntry{
		info:         info,
		configPath:   configPath,
		idleTimeout:  idleTimeout,
		lastActiveAt: lastActiveAt,
		conn:         conn,
		outbox:       make(chan *Envelope, outboxCapacity),
		closeCh:      make(chan struct{}),
	}
}

// Send enqueues env for asynchronous delivery on the entry's writer
// goroutine. It blocks until the envelope is queued, the entry is closed,
// or the supplied timeout elapses. It returns false when the envelope was
// not queued (closed entry or full outbox after the timeout).
func (e *connEntry) Send(env *Envelope, timeout time.Duration) bool {
	select {
	case <-e.closeCh:
		return false
	default:
	}

	if timeout <= 0 {
		select {
		case e.outbox <- env:
			return true
		case <-e.closeCh:
			return false
		}
	}

	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case e.outbox <- env:
		return true
	case <-e.closeCh:
		return false
	case <-timer.C:
		return false
	}
}

// Close marks the entry as closed and signals its writer goroutine to
// exit. Close is idempotent.
func (e *connEntry) Close() {
	e.once.Do(func() {
		close(e.closeCh)
	})
}

// Conn exposes the underlying network connection. Callers must not use it
// for direct writes once the entry has a writer goroutine attached; use
// Send instead.
func (e *connEntry) Conn() net.Conn {
	return e.conn
}

// Info returns a snapshot of the entry's workspace info.
func (e *connEntry) Info() types.WorkspaceInfo {
	return e.info
}

type Registry struct {
	mu      sync.RWMutex
	entries map[string]*connEntry
}

type RegisteredWorkspace struct {
	Info         types.WorkspaceInfo
	ConfigPath   string
	IdleTimeout  time.Duration
	LastActiveAt time.Time
}

func NewRegistry() *Registry {
	return &Registry{
		entries: make(map[string]*connEntry),
	}
}

// Register inserts a fresh connEntry for name. If a previous entry exists
// for the same name and a different connection, that previous entry (and
// its underlying connection) is returned so the caller can close it. The
// new entry is returned for the caller to attach a writer goroutine to.
func (r *Registry) Register(name, dir, description, configPath string, idleTimeout time.Duration, conn net.Conn) (entry *connEntry, previous *connEntry) {
	r.mu.Lock()
	defer r.mu.Unlock()
	now := time.Now()
	statusText := ""
	if existing, ok := r.entries[name]; ok {
		if existing.conn != conn {
			previous = existing
		}
		statusText = existing.info.StatusText
	}
	entry = newConnEntry(types.WorkspaceInfo{
		Name:        name,
		Dir:         dir,
		Description: description,
		Status:      types.StatusOnline,
		StatusText:  statusText,
		ConnectedAt: &now,
	}, configPath, idleTimeout, now, conn)
	r.entries[name] = entry
	return entry, previous
}

func (r *Registry) Unregister(name string) {
	r.mu.Lock()
	entry := r.entries[name]
	delete(r.entries, name)
	r.mu.Unlock()
	if entry != nil {
		entry.Close()
	}
}

func (r *Registry) UnregisterIfConn(name string, conn net.Conn) bool {
	r.mu.Lock()
	entry, ok := r.entries[name]
	if !ok || entry.conn != conn {
		r.mu.Unlock()
		return false
	}
	delete(r.entries, name)
	r.mu.Unlock()
	entry.Close()
	return true
}

func (r *Registry) Get(name string) (*connEntry, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	entry, ok := r.entries[name]
	return entry, ok
}

func (r *Registry) List() []types.WorkspaceInfo {
	r.mu.RLock()
	defer r.mu.RUnlock()
	result := make([]types.WorkspaceInfo, 0, len(r.entries))
	for _, entry := range r.entries {
		result = append(result, entry.info)
	}
	return result
}

func (r *Registry) SetStatus(name, status string) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	entry, ok := r.entries[name]
	if !ok {
		return false
	}
	entry.info.StatusText = status
	return true
}

func (r *Registry) Touch(name string) bool {
	if r == nil {
		return false
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	entry, ok := r.entries[name]
	if !ok {
		return false
	}
	entry.lastActiveAt = time.Now()
	return true
}

func (r *Registry) Snapshot() []RegisteredWorkspace {
	r.mu.RLock()
	defer r.mu.RUnlock()
	result := make([]RegisteredWorkspace, 0, len(r.entries))
	for _, entry := range r.entries {
		result = append(result, RegisteredWorkspace{
			Info:         entry.info,
			ConfigPath:   entry.configPath,
			IdleTimeout:  entry.idleTimeout,
			LastActiveAt: entry.lastActiveAt,
		})
	}
	return result
}

func (r *Registry) FindByConn(conn net.Conn) (string, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	for name, entry := range r.entries {
		if entry.conn == conn {
			return name, true
		}
	}
	return "", false
}
