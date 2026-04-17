package cmd

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/daemonutil"
	"github.com/spf13/cobra"
)

var daemonCmd = &cobra.Command{
	Use:   "daemon",
	Short: "Manage the ax daemon",
}

var daemonStartCmd = &cobra.Command{
	Use:   "start",
	Short: "Start the ax daemon",
	RunE: func(cmd *cobra.Command, args []string) error {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()

		sigs := make(chan os.Signal, 1)
		signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
		go func() {
			<-sigs
			cancel()
		}()

		d := daemon.New(socketPath)
		return d.Run(ctx)
	},
}

var daemonStopCmd = &cobra.Command{
	Use:   "stop",
	Short: "Stop the ax daemon",
	RunE: func(cmd *cobra.Command, args []string) error {
		pidPath := filepath.Join(filepath.Dir(daemonutil.ExpandSocketPath(socketPath)), "daemon.pid")
		data, err := os.ReadFile(pidPath)
		if err != nil {
			return fmt.Errorf("daemon not running (no pid file)")
		}
		pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
		if err != nil {
			return fmt.Errorf("invalid pid file")
		}
		proc, err := os.FindProcess(pid)
		if err != nil {
			return fmt.Errorf("process not found: %w", err)
		}
		if err := proc.Signal(syscall.SIGTERM); err != nil {
			return fmt.Errorf("signal: %w", err)
		}
		fmt.Printf("Sent SIGTERM to daemon (pid %d)\n", pid)
		return nil
	},
}

var daemonStatusCmd = &cobra.Command{
	Use:   "status",
	Short: "Show daemon status",
	RunE: func(cmd *cobra.Command, args []string) error {
		sp := daemonutil.ExpandSocketPath(socketPath)
		pidPath := filepath.Join(filepath.Dir(sp), "daemon.pid")

		data, err := os.ReadFile(pidPath)
		if err != nil {
			fmt.Println("Daemon: not running")
			return nil
		}
		pid, _ := strconv.Atoi(strings.TrimSpace(string(data)))
		proc, err := os.FindProcess(pid)
		if err != nil {
			fmt.Println("Daemon: not running (stale pid)")
			return nil
		}
		// Check if process is alive
		if err := proc.Signal(syscall.Signal(0)); err != nil {
			fmt.Println("Daemon: not running (stale pid)")
			return nil
		}
		fmt.Printf("Daemon: running (pid %d)\n", pid)
		fmt.Printf("Socket: %s\n", sp)
		return nil
	},
}

func init() {
	daemonCmd.AddCommand(daemonStartCmd, daemonStopCmd, daemonStatusCmd)
	rootCmd.AddCommand(daemonCmd)
}
