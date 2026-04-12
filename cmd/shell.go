package cmd

import (
	"fmt"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
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

		sp := daemon.ExpandSocketPath(socketPath)

		if !isDaemonRunning(sp) {
			return fmt.Errorf("daemon is not running — run 'ax up' first")
		}

		tree, err := config.LoadTree(cfgPath)
		if err != nil {
			return fmt.Errorf("load config tree: %w", err)
		}

		// Ensure the root orchestrator session exists (and any sub-orchestrators)
		if err := ensureOrchestrators(tree, sp, cfgPath); err != nil {
			return err
		}

		orchSessionName := tmux.SessionName("orchestrator")

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
