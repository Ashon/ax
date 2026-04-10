package cmd

import (
	"fmt"

	"github.com/ashon/amux/internal/daemon"
	"github.com/ashon/amux/internal/tmux"
	"github.com/spf13/cobra"
)

var statusCmd = &cobra.Command{
	Use:   "status",
	Short: "Show overall amux status",
	RunE: func(cmd *cobra.Command, args []string) error {
		sp := daemon.ExpandSocketPath(socketPath)

		// Daemon status
		if isDaemonRunning(sp) {
			fmt.Println("Daemon: running")
		} else {
			fmt.Println("Daemon: stopped")
		}

		// Workspace status
		sessions, err := tmux.ListSessions()
		if err != nil {
			return err
		}

		fmt.Printf("\nWorkspaces: %d active\n", len(sessions))
		if len(sessions) > 0 {
			fmt.Printf("\n%-20s %-10s %s\n", "NAME", "STATUS", "SESSION")
			fmt.Printf("%-20s %-10s %s\n", "----", "------", "-------")
			for _, s := range sessions {
				status := "detached"
				if s.Attached {
					status = "attached"
				}
				fmt.Printf("%-20s %-10s %s\n", s.Workspace, status, s.Name)
			}
		}

		return nil
	},
}

func init() {
	rootCmd.AddCommand(statusCmd)
}
