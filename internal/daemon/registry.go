package daemon

import (
	"net"
	"sync"
	"time"

	"github.com/ashon/ax/internal/types"
)

type connEntry struct {
	info types.WorkspaceInfo
	conn net.Conn
	mu   sync.Mutex // guards writes to conn
}

type Registry struct {
	mu      sync.RWMutex
	entries map[string]*connEntry
}

func NewRegistry() *Registry {
	return &Registry{
		entries: make(map[string]*connEntry),
	}
}

func (r *Registry) Register(name, dir, description string, conn net.Conn) {
	r.mu.Lock()
	defer r.mu.Unlock()
	now := time.Now()
	r.entries[name] = &connEntry{
		info: types.WorkspaceInfo{
			Name:        name,
			Dir:         dir,
			Description: description,
			Status:      types.StatusOnline,
			ConnectedAt: &now,
		},
		conn: conn,
	}
}

func (r *Registry) Unregister(name string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.entries, name)
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
