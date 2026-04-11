package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"syscall"
	"time"

	"path/filepath"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
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
		exe, err := os.Executable()
		if err != nil {
			return fmt.Errorf("resolve ax binary: %w", err)
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

		// Write .mcp.json for user's local claude session (registers as "user")
		configDir := config.ConfigRootDir(cfgPath)
		sp := daemon.ExpandSocketPath(socketPath)
		if err := workspace.WriteMCPConfig(configDir, "user", sp, cfgPath); err != nil {
			return fmt.Errorf("write user mcp config: %w", err)
		}
		claudeUserCommand, err := agent.BuildUserCommand(agent.RuntimeClaude, configDir, "user", sp, exe, cfgPath)
		if err != nil {
			return fmt.Errorf("build claude user command: %w", err)
		}
		codexUserCommand, err := agent.BuildUserCommand(agent.RuntimeCodex, configDir, "user", sp, exe, cfgPath)
		if err != nil {
			return fmt.Errorf("build codex user command: %w", err)
		}

		// Create orchestrator in ~/.ax/orchestrator
		home, _ := os.UserHomeDir()
		orchDir := filepath.Join(home, ".ax", "orchestrator")
		os.MkdirAll(orchDir, 0o755)
		os.MkdirAll(filepath.Join(orchDir, ".claude"), 0o755) // pre-create to skip trust prompt

		orchRuntime := agent.NormalizeRuntime(cfg.OrchestratorRuntime)
		if _, err := agent.Get(orchRuntime); err != nil {
			return fmt.Errorf("invalid orchestrator runtime: %w", err)
		}

		if !tmux.SessionExists("orchestrator") {
			if err := workspace.WriteMCPConfig(orchDir, "orchestrator", sp, cfgPath); err != nil {
				return fmt.Errorf("write orchestrator mcp config: %w", err)
			}
			if err := workspace.WriteOrchestratorPrompt(orchDir, cfg, orchRuntime); err != nil {
				return fmt.Errorf("write orchestrator prompt: %w", err)
			}
			if err := tmux.CreateSessionWithArgs("orchestrator", orchDir, []string{
				exe,
				"run-agent",
				"--runtime", orchRuntime,
				"--workspace", "orchestrator",
				"--socket", sp,
				"--config", cfgPath,
			}); err != nil {
				return fmt.Errorf("create orchestrator session: %w", err)
			}
			fmt.Printf("\nOrchestrator: started (~/.ax/orchestrator, runtime: %s)\n", orchRuntime)
		} else {
			fmt.Printf("\nOrchestrator: already running\n")
		}

		fmt.Println("User session:")
		fmt.Printf("  Claude: run %s\n", claudeUserCommand)
		fmt.Printf("  Codex:  run %s\n", codexUserCommand)
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
