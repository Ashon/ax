package cmd

import (
	"fmt"
	"os"

	"github.com/ashon/amux/internal/config"
	"github.com/ashon/amux/internal/tmux"
	"github.com/ashon/amux/internal/workspace"
	"github.com/spf13/cobra"
)

var workspaceCmd = &cobra.Command{
	Use:     "workspace",
	Aliases: []string{"ws"},
	Short:   "Manage workspaces",
}

var wsCreateDir string

var wsCreateCmd = &cobra.Command{
	Use:   "create <name>",
	Short: "Create a new workspace",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		name := args[0]
		dir := wsCreateDir
		if dir == "" {
			var err error
			dir, err = os.Getwd()
			if err != nil {
				return err
			}
		}

		mgr := workspace.NewManager(socketPath)
		if err := mgr.Create(name, config.Workspace{Dir: dir}); err != nil {
			return err
		}

		fmt.Printf("Workspace %q created (session: %s, dir: %s)\n", name, tmux.SessionName(name), dir)
		fmt.Printf("Attach with: amux workspace attach %s\n", name)
		return nil
	},
}

var wsDestroyCmd = &cobra.Command{
	Use:   "destroy <name>",
	Short: "Destroy a workspace",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		name := args[0]
		mgr := workspace.NewManager(socketPath)
		if err := mgr.Destroy(name, ""); err != nil {
			return err
		}
		fmt.Printf("Workspace %q destroyed\n", name)
		return nil
	},
}

var wsListCmd = &cobra.Command{
	Use:   "list",
	Short: "List active workspaces",
	RunE: func(cmd *cobra.Command, args []string) error {
		sessions, err := tmux.ListSessions()
		if err != nil {
			return err
		}

		if len(sessions) == 0 {
			fmt.Println("No active workspaces.")
			return nil
		}

		fmt.Printf("%-20s %-10s %s\n", "WORKSPACE", "STATUS", "SESSION")
		fmt.Printf("%-20s %-10s %s\n", "---------", "------", "-------")
		for _, s := range sessions {
			status := "detached"
			if s.Attached {
				status = "attached"
			}
			fmt.Printf("%-20s %-10s %s\n", s.Workspace, status, s.Name)
		}
		return nil
	},
}

var wsAttachCmd = &cobra.Command{
	Use:   "attach <name>",
	Short: "Attach to a workspace tmux session",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		name := args[0]
		if !tmux.SessionExists(name) {
			return fmt.Errorf("workspace %q not found (no tmux session %s)", name, tmux.SessionName(name))
		}
		return tmux.AttachSession(name)
	},
}

func init() {
	wsCreateCmd.Flags().StringVar(&wsCreateDir, "dir", "", "workspace directory (default: current dir)")
	workspaceCmd.AddCommand(wsCreateCmd, wsDestroyCmd, wsListCmd, wsAttachCmd)
	rootCmd.AddCommand(workspaceCmd)
}
