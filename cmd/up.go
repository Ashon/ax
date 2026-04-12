package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"syscall"
	"time"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/workspace"
	"github.com/spf13/cobra"
)

var upCmd = &cobra.Command{
	Use:   "up",
	Short: "Start daemon and all workspaces defined in ax config",
	RunE: func(cmd *cobra.Command, args []string) error {
		cfgPath, err := resolveConfigPath()
		if err != nil {
			return err
		}
		cfg, err := config.Load(cfgPath)
		if err != nil {
			return err
		}

		fmt.Printf("Project: %s\n\n", cfg.Project)

		// Ensure daemon is running
		if err := ensureDaemon(); err != nil {
			return fmt.Errorf("start daemon: %w", err)
		}
		fmt.Println("Daemon: running")

		// Create workspaces
		fmt.Println("\nWorkspaces:")
		mgr := workspace.NewManager(socketPath, cfgPath)
		if err := mgr.CreateAll(cfg); err != nil {
			return err
		}

		// Create orchestrator sessions for the full project tree
		tree, err := config.LoadTree(cfgPath)
		if err != nil {
			return fmt.Errorf("load config tree: %w", err)
		}
		sp := daemon.ExpandSocketPath(socketPath)
		fmt.Println("\nOrchestrators:")
		if err := ensureOrchestrators(tree, sp, cfgPath); err != nil {
			return err
		}

		fmt.Println("\nRun 'ax shell' to start a conversation with the root orchestrator.")
		return nil
	},
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

	// Quick signal check via PID file
	pidPath := socketPath[:len(socketPath)-len("daemon.sock")] + "daemon.pid"
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
