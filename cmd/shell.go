package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/workspace"
	"github.com/spf13/cobra"
)

var shellCmd = &cobra.Command{
	Use:   "shell",
	Short: "Start an interactive session with the root orchestrator",
	Long:  "Launches the orchestrator in a TUI with agent status sidebar and message stream.",
	RunE: func(cmd *cobra.Command, args []string) error {
		cfgPath, err := resolveConfigPath()
		if err != nil {
			return err
		}
		cfg, err := config.Load(cfgPath)
		if err != nil {
			return err
		}

		sp := daemon.ExpandSocketPath(socketPath)

		if !isDaemonRunning(sp) {
			return fmt.Errorf("daemon is not running — run 'ax up' first")
		}

		orchRuntime := agent.NormalizeRuntime(cfg.OrchestratorRuntime)
		if _, err := agent.Get(orchRuntime); err != nil {
			return fmt.Errorf("invalid orchestrator runtime: %w", err)
		}

		home, _ := os.UserHomeDir()
		orchDir := filepath.Join(home, ".ax", "orchestrator")
		os.MkdirAll(orchDir, 0o755)
		os.MkdirAll(filepath.Join(orchDir, ".claude"), 0o755)

		if err := workspace.WriteMCPConfig(orchDir, "orchestrator", sp, cfgPath); err != nil {
			return fmt.Errorf("write orchestrator mcp config: %w", err)
		}
		if err := workspace.WriteOrchestratorPrompt(orchDir, cfg, orchRuntime); err != nil {
			return fmt.Errorf("write orchestrator prompt: %w", err)
		}

		// Create orchestrator tmux session if not already running
		orchSessionName := tmux.SessionName("orchestrator")
		if !tmux.SessionExists("orchestrator") {
			exe, err := os.Executable()
			if err != nil {
				return fmt.Errorf("resolve ax binary: %w", err)
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
		}

		// Launch TUI
		model := newShellModel(orchSessionName, sp)
		p := tea.NewProgram(model, tea.WithAltScreen())
		_, err = p.Run()
		return err
	},
}

func init() {
	rootCmd.AddCommand(shellCmd)
}
