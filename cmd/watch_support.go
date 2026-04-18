package cmd

import (
	"fmt"

	"github.com/ashon/ax/internal/mcpserver"
)

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
