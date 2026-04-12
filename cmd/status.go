package cmd

import (
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/spf13/cobra"
)

var statusCmd = &cobra.Command{
	Use:   "status",
	Short: "Show overall ax status",
	RunE: func(cmd *cobra.Command, args []string) error {
		sp := daemon.ExpandSocketPath(socketPath)

		if isDaemonRunning(sp) {
			fmt.Println("Daemon: running")
		} else {
			fmt.Println("Daemon: stopped")
		}

		sessions, err := tmux.ListSessions()
		if err != nil {
			return err
		}
		sessionByWorkspace := make(map[string]tmux.SessionInfo, len(sessions))
		for _, s := range sessions {
			sessionByWorkspace[s.Workspace] = s
		}

		fmt.Printf("\nWorkspaces: %d active\n\n", len(sessions))

		// Try to render as a config tree, then list any unregistered sessions
		cfgPath, cfgErr := resolveConfigPath()
		if cfgErr == nil {
			if tree, err := config.LoadTree(cfgPath); err == nil && tree != nil {
				known := make(map[string]bool)
				collectKnownWorkspaces(tree, known)
				printProjectTree(tree, 0, sessionByWorkspace)

				// Any running sessions not in the tree
				var unregistered []tmux.SessionInfo
				for _, s := range sessions {
					if !known[s.Workspace] {
						unregistered = append(unregistered, s)
					}
				}
				if len(unregistered) > 0 {
					fmt.Println("\n▾ unregistered (not in config tree)")
					for _, s := range unregistered {
						status := "detached"
						if s.Attached {
							status = "attached"
						}
						fmt.Printf("  ● %-26s %-10s %s\n", s.Workspace, status, s.Name)
					}
					fmt.Println("\nRun 'ax init' in the project directory to register these.")
				}
				return nil
			}
		}

		// Fallback: flat list
		if len(sessions) > 0 {
			fmt.Printf("%-24s %-10s %s\n", "NAME", "STATUS", "SESSION")
			fmt.Printf("%-24s %-10s %s\n", "----", "------", "-------")
			for _, s := range sessions {
				status := "detached"
				if s.Attached {
					status = "attached"
				}
				fmt.Printf("%-24s %-10s %s\n", s.Workspace, status, s.Name)
			}
		}
		return nil
	},
}

func collectKnownWorkspaces(node *config.ProjectNode, known map[string]bool) {
	if node == nil {
		return
	}
	orchName := "orchestrator"
	if node.Prefix != "" {
		orchName = node.Prefix + ".orchestrator"
	}
	known[orchName] = true
	for _, ws := range node.Workspaces {
		known[ws.MergedName] = true
	}
	for _, child := range node.Children {
		collectKnownWorkspaces(child, known)
	}
}

func printProjectTree(node *config.ProjectNode, level int, sessionByWorkspace map[string]tmux.SessionInfo) {
	if node == nil {
		return
	}
	indent := strings.Repeat("  ", level)
	fmt.Printf("%s▾ %s\n", indent, node.Name)

	orchName := "orchestrator"
	if node.Prefix != "" {
		orchName = node.Prefix + ".orchestrator"
	}
	printLeaf(level+1, "◆ orchestrator", orchName, sessionByWorkspace)

	for _, ws := range node.Workspaces {
		printLeaf(level+1, ws.Name, ws.MergedName, sessionByWorkspace)
	}

	for _, child := range node.Children {
		printProjectTree(child, level+1, sessionByWorkspace)
	}
}

func printLeaf(level int, label, mergedName string, sessionByWorkspace map[string]tmux.SessionInfo) {
	indent := strings.Repeat("  ", level)
	if s, ok := sessionByWorkspace[mergedName]; ok {
		status := "detached"
		if s.Attached {
			status = "attached"
		}
		fmt.Printf("%s● %-26s %-10s %s\n", indent, label, status, s.Name)
	} else {
		fmt.Printf("%s○ %-26s %-10s\n", indent, label, "offline")
	}
}

func init() {
	rootCmd.AddCommand(statusCmd)
}
