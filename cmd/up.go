package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"sort"
	"syscall"
	"time"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/workspace"
	"github.com/spf13/cobra"
)

var (
	upEnsureDaemon                 = ensureDaemon
	upEnsureWorkspaceArtifacts     = workspace.EnsureArtifacts
	upRefreshOrchestratorArtifacts = refreshOrchestratorArtifacts
	upRootOrchestratorDisabled     = rootOrchestratorDisabled
)

var upCmd = &cobra.Command{
	Use:   "up",
	Short: "Start daemon and prepare on-demand workspace/orchestrator artifacts",
	RunE: func(cmd *cobra.Command, args []string) error {
		cfgPath, err := resolveConfigPath()
		if err != nil {
			return err
		}
		cfg, err := config.Load(cfgPath)
		if err != nil {
			return err
		}
		tree, err := config.LoadTree(cfgPath)
		if err != nil {
			return fmt.Errorf("load config tree: %w", err)
		}

		fmt.Printf("Project: %s\n\n", cfg.Project)

		// Ensure daemon is running
		if err := upEnsureDaemon(); err != nil {
			return fmt.Errorf("start daemon: %w", err)
		}
		fmt.Println("Daemon: running")

		// Prepare workspace artifacts for on-demand dispatch.
		fmt.Println("\nWorkspaces:")
		if err := prepareOnDemandWorkspaces(cfg, cfgPath); err != nil {
			return err
		}

		// Prepare orchestrator artifacts without starting sessions.
		sp := daemon.ExpandSocketPath(socketPath)
		fmt.Println("\nOrchestrators:")
		if err := upRefreshOrchestratorArtifacts(tree, "", sp, cfgPath); err != nil {
			return err
		}
		fmt.Println("  tree: ready (on-demand)")

		disabledRoot, err := upRootOrchestratorDisabled(cfgPath)
		if err != nil {
			return err
		}
		if disabledRoot {
			fmt.Println("\nManaged root orchestrator state is disabled by config.")
			fmt.Println("Workspace and child/project orchestrator agents will start on demand when work is dispatched.")
			fmt.Println("Run 'ax claude' or 'ax codex' to launch a foreground root orchestrator manually.")
			return nil
		}

		fmt.Println("\nRun 'ax claude' or 'ax codex' to launch the root orchestrator CLI.")
		fmt.Println("Workspace and child/project orchestrator agents will start on demand when messages or tasks are dispatched.")
		return nil
	},
}

func prepareOnDemandWorkspaces(cfg *config.Config, cfgPath string) error {
	names := make([]string, 0, len(cfg.Workspaces))
	for name := range cfg.Workspaces {
		names = append(names, name)
	}
	sort.Strings(names)

	for _, name := range names {
		ws := cfg.Workspaces[name]
		if err := upEnsureWorkspaceArtifacts(name, ws, socketPath, cfgPath); err != nil {
			return fmt.Errorf("prepare workspace %q: %w", name, err)
		}
		fmt.Printf("  %s: ready (on-demand, dir: %s)\n", name, ws.Dir)
	}
	return nil
}

func ensureDaemon() error {
	sp := daemon.ExpandSocketPath(socketPath)

	// Check if already running
	if isDaemonRunning(sp) {
		return nil
	}

	// Start daemon in background
	exe, err := os.Executable()
	if err != nil {
		return err
	}

	proc := exec.Command(exe, "daemon", "start", "--socket", socketPath)
	proc.Stdout = nil
	proc.Stderr = nil
	proc.SysProcAttr = &syscall.SysProcAttr{Setsid: true}

	if err := proc.Start(); err != nil {
		return fmt.Errorf("start daemon process: %w", err)
	}

	// Detach — don't wait for it
	proc.Process.Release()

	// Wait for socket to appear
	for i := 0; i < 30; i++ {
		if _, err := os.Stat(sp); err == nil {
			return nil
		}
		time.Sleep(100 * time.Millisecond)
	}

	return fmt.Errorf("daemon did not start within 3s")
}

func isDaemonRunning(socketPath string) bool {
	// Try connecting to the socket
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	_ = ctx

	if _, err := os.Stat(socketPath); err != nil {
		return false
	}

	// Quick signal check via the daemon pid file in the socket directory.
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

func init() {
	rootCmd.AddCommand(upCmd)

	// Also handle Ctrl+C gracefully for 'up' by ignoring it
	// (daemon runs independently)
	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT)
}
