package cmd

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"github.com/ashon/ax/internal/mcpserver"
)

// isDaemonRunning checks for a live daemon via its pid file. Used by
// the watch TUI to decide whether to render live state. The Rust
// ax-cli owns the canonical daemon lifecycle now; this helper stays
// read-only and runtime-cheap.
func isDaemonRunning(socketPath string) bool {
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	_ = ctx

	if _, err := os.Stat(socketPath); err != nil {
		return false
	}
	pidPath := filepath.Join(filepath.Dir(socketPath), "daemon.pid")
	data, err := os.ReadFile(pidPath)
	if err != nil {
		return false
	}
	var pid int
	fmt.Sscanf(string(data), "%d", &pid)
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false
	}
	return proc.Signal(syscall.Signal(0)) == nil
}

// newCLIClient returns a shared ax daemon client used by the
// remaining Go surfaces (currently just the watch TUI). The task
// subcommand previously owned this helper; it's kept here so the
// TUI keeps compiling until its own port lands.
func newCLIClient() (*mcpserver.DaemonClient, error) {
	client := mcpserver.NewDaemonClient(socketPath, "_cli")
	if err := client.Connect(); err != nil {
		return nil, fmt.Errorf("connect to daemon: %w (is daemon running?)", err)
	}
	return client, nil
}

// activityKindLabel maps an in-process taskActivityKind value to
// its display label. The Rust port (tasks.rs) owns the canonical
// implementation; this shim is kept so watch_streams.go compiles
// until the TUI port replaces it.
func activityKindLabel(kind taskActivityKind) string {
	switch kind {
	case taskActivityLifecycle:
		return "lifecycle"
	case taskActivityLog:
		return "log"
	case taskActivityMessage:
		return "message"
	default:
		return "activity"
	}
}
