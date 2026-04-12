package cmd

import (
	"fmt"
	"path/filepath"
	"strconv"
	"strings"

	"os"
	"syscall"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/workspace"
	"github.com/spf13/cobra"
)

var downCmd = &cobra.Command{
	Use:   "down",
	Short: "Stop all workspaces and the daemon",
	RunE: func(cmd *cobra.Command, args []string) error {
		cfgPath, err := resolveConfigPath()
		if err != nil {
			return err
		}
		cfg, err := config.Load(cfgPath)
		if err != nil {
			return err
		}

		fmt.Println("Stopping workspaces:")
		mgr := workspace.NewManager(socketPath, cfgPath)
		if err := mgr.DestroyAll(cfg); err != nil {
			return err
		}

		// Stop orchestrator sessions (root + any sub-orchestrators)
		if tree, err := config.LoadTree(cfgPath); err == nil {
			fmt.Println("\nStopping orchestrators:")
			destroyOrchestrators(tree)
		} else if tmux.SessionExists("orchestrator") {
			tmux.DestroySession("orchestrator")
			fmt.Println("  orchestrator: stopped")
		}

		// Remove orchestrator .mcp.json
		configDir := filepath.Dir(cfgPath)
		workspace.RemoveMCPConfig(configDir)

		// Stop daemon
		sp := daemon.ExpandSocketPath(socketPath)
		pidPath := filepath.Join(filepath.Dir(sp), "daemon.pid")
		data, err := os.ReadFile(pidPath)
		if err == nil {
			pid, _ := strconv.Atoi(strings.TrimSpace(string(data)))
			if proc, err := os.FindProcess(pid); err == nil {
				proc.Signal(syscall.SIGTERM)
				fmt.Println("\nDaemon: stopped")
			}
		} else {
			fmt.Println("\nDaemon: not running")
		}

		return nil
	},
}

func init() {
	rootCmd.AddCommand(downCmd)
}
