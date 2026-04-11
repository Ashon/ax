package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/workspace"
	"github.com/spf13/cobra"
)

var shellCmd = &cobra.Command{
	Use:   "shell",
	Short: "Start an interactive session with the root orchestrator",
	Long:  "Launches the orchestrator agent in the current terminal, connecting to the running daemon and workspaces.",
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

		// Ensure daemon is running
		if !isDaemonRunning(sp) {
			return fmt.Errorf("daemon is not running — run 'ax up' first")
		}

		// Resolve orchestrator runtime
		orchRuntime := agent.NormalizeRuntime(cfg.OrchestratorRuntime)
		if _, err := agent.Get(orchRuntime); err != nil {
			return fmt.Errorf("invalid orchestrator runtime: %w", err)
		}

		// Prepare orchestrator directory
		home, _ := os.UserHomeDir()
		orchDir := filepath.Join(home, ".ax", "orchestrator")
		os.MkdirAll(orchDir, 0o755)
		os.MkdirAll(filepath.Join(orchDir, ".claude"), 0o755)

		// Write MCP config and orchestrator prompt
		if err := workspace.WriteMCPConfig(orchDir, "orchestrator", sp, cfgPath); err != nil {
			return fmt.Errorf("write orchestrator mcp config: %w", err)
		}
		if err := workspace.WriteOrchestratorPrompt(orchDir, cfg, orchRuntime); err != nil {
			return fmt.Errorf("write orchestrator prompt: %w", err)
		}

		// Launch orchestrator in foreground
		return agent.RunInDir(orchRuntime, orchDir, "orchestrator", sp, cfgPath)
	},
}

func init() {
	rootCmd.AddCommand(shellCmd)
}
