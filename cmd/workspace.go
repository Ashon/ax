package cmd

import (
	"fmt"
	"os"
	"strings"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/workspace"
	"github.com/spf13/cobra"
)

var workspaceCmd = &cobra.Command{
	Use:     "workspace",
	Aliases: []string{"ws"},
	Short:   "Manage workspaces",
}

var (
	wsSessionExists = tmux.SessionExists
	wsAttachSession = tmux.AttachSession
)

var wsCreateDir string
var wsListInternal bool

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

		mgr := workspace.NewManager(socketPath, configPath)
		if err := mgr.Create(name, config.Workspace{Dir: dir}); err != nil {
			return err
		}

		fmt.Printf("Workspace %q created (session: %s, dir: %s)\n", name, tmux.SessionName(name), dir)
		fmt.Printf("Attach with: ax workspace attach %s\n", name)
		return nil
	},
}

var wsDestroyCmd = &cobra.Command{
	Use:   "destroy <name>",
	Short: "Destroy a workspace",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		name := args[0]
		mgr := workspace.NewManager(socketPath, configPath)
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
	Long:  "List the union of tmux sessions and daemon-registered workspaces. Internal daemon-only identities such as `_cli` are hidden by default; use `--internal` to show them.",
	RunE: func(cmd *cobra.Command, args []string) error {
		sessions, err := tmux.ListSessions()
		if err != nil {
			return err
		}

		// Try to load config for descriptions
		descriptions := map[string]string{}
		desired := map[string]bool{}
		reconfigureEnabled := false
		if cfgPath, err := resolveConfigPath(); err == nil {
			if cfg, err := config.Load(cfgPath); err == nil {
				for name, ws := range cfg.Workspaces {
					descriptions[name] = ws.Description
					desired[name] = true
				}
			}
			if topology, err := loadTeamReconfigureTopology(cfgPath); err == nil {
				reconfigureEnabled = topology.Enabled
			}
		}

		workspaceInfos := map[string]types.WorkspaceInfo{}
		if sp := daemon.ExpandSocketPath(socketPath); isDaemonRunning(sp) {
			if client, err := newCLIClient(); err == nil {
				if workspaces, err := client.ListWorkspaces(); err == nil {
					workspaceInfos = workspaceInfoMap(workspaces)
				}
				client.Close()
			}
		}

		view := buildWorkspaceListRows(sessions, workspaceInfos, descriptions, desired, reconfigureEnabled, wsListInternal)
		if len(view.Rows) == 0 {
			fmt.Println("No workspaces found.")
			if note := formatHiddenInternalWorkspaceNote(view.HiddenInternal); note != "" {
				fmt.Println(note)
			}
			return nil
		}

		if view.ReconfigureEnabled {
			fmt.Printf("%-22s %-14s %-10s %-8s %-40s %s\n", "WORKSPACE", "RECONCILE", "TMUX", "AGENT", "STATUS TEXT", "DESCRIPTION")
			fmt.Printf("%-22s %-14s %-10s %-8s %-40s %s\n", "---------", "---------", "----", "-----", "-----------", "-----------")
			for _, row := range view.Rows {
				fmt.Printf("%-22s %-14s %-10s %-8s %-40s %s\n",
					row.Name,
					row.Reconcile,
					row.Tmux,
					row.Agent,
					row.StatusText,
					row.Description,
				)
			}
		} else {
			fmt.Printf("%-22s %-10s %-8s %-40s %s\n", "WORKSPACE", "TMUX", "AGENT", "STATUS TEXT", "DESCRIPTION")
			fmt.Printf("%-22s %-10s %-8s %-40s %s\n", "---------", "----", "-----", "-----------", "-----------")
			for _, row := range view.Rows {
				fmt.Printf("%-22s %-10s %-8s %-40s %s\n",
					row.Name,
					row.Tmux,
					row.Agent,
					row.StatusText,
					row.Description,
				)
			}
		}
		if view.ReconfigureEnabled {
			fmt.Println("\nReconcile state is relative to the active config tree.")
		}
		if note := formatHiddenInternalWorkspaceNote(view.HiddenInternal); note != "" {
			fmt.Printf("\n%s\n", note)
		}
		return nil
	},
}

var wsAttachCmd = &cobra.Command{
	Use:   "attach <name>",
	Short: "Attach to a workspace or project orchestrator tmux session",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		name := args[0]
		target := resolveWorkspaceAttachTarget(name)
		if target == "" {
			orchestratorName := workspace.OrchestratorName(strings.TrimSpace(name))
			return fmt.Errorf("workspace %q not found (no tmux session %s; tried project orchestrator %q -> %s)", name, tmux.SessionName(name), orchestratorName, tmux.SessionName(orchestratorName))
		}
		return wsAttachSession(target)
	},
}

var wsInterruptCmd = &cobra.Command{
	Use:   "interrupt <name>",
	Short: "Interrupt the current interactive agent action",
	Long:  "Sends Escape to the workspace tmux session to interrupt the current agent CLI action without killing the session.",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		name := args[0]
		if !wsSessionExists(name) {
			return fmt.Errorf("workspace %q not found (no tmux session %s)", name, tmux.SessionName(name))
		}
		if err := tmux.InterruptWorkspace(name); err != nil {
			return err
		}
		fmt.Printf("Workspace %q interrupted\n", name)
		return nil
	},
}

func init() {
	wsCreateCmd.Flags().StringVar(&wsCreateDir, "dir", "", "workspace directory (default: current dir)")
	wsListCmd.Flags().BoolVar(&wsListInternal, "internal", false, "include internal daemon-only identities such as _cli")
	workspaceCmd.AddCommand(wsCreateCmd, wsDestroyCmd, wsListCmd, wsAttachCmd, wsInterruptCmd)
	rootCmd.AddCommand(workspaceCmd)
}

func resolveWorkspaceAttachTarget(name string) string {
	name = strings.TrimSpace(name)
	if name == "" {
		return ""
	}
	if wsSessionExists(name) {
		return name
	}
	orchestratorName := workspace.OrchestratorName(name)
	if orchestratorName != name && wsSessionExists(orchestratorName) {
		return orchestratorName
	}
	return ""
}
