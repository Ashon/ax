package cmd

import (
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/spf13/cobra"
)

var statusCmd = &cobra.Command{
	Use:   "status",
	Short: "Show overall ax status",
	RunE: func(cmd *cobra.Command, args []string) error {
		sp := daemonutil.ExpandSocketPath(socketPath)
		daemonRunning := isDaemonRunning(sp)
		taskSummary := taskSummary{}
		workspaceInfos := map[string]types.WorkspaceInfo{}

		if daemonRunning {
			fmt.Println("Daemon: running")
		} else {
			fmt.Println("Daemon: stopped")
		}
		if daemonRunning {
			if client, err := newCLIClient(); err == nil {
				tasks, taskErr := client.ListTasks("", "", nil)
				workspaces, wsErr := client.ListWorkspaces()
				client.Close()
				if taskErr == nil {
					taskSummary = summarizeTasks(tasks)
					fmt.Printf("Tasks: %s\n", formatTaskSummary(taskSummary))
				} else {
					fmt.Printf("Tasks: unavailable (%v)\n", taskErr)
				}
				if wsErr == nil {
					workspaceInfos = workspaceInfoMap(workspaces)
					fmt.Printf("Agents: %d online\n", len(workspaceInfos))
				} else {
					fmt.Printf("Agents: unavailable (%v)\n", wsErr)
				}
				if hint := taskAttentionHint(taskSummary); hint != "" {
					fmt.Println(hint)
				}
			} else {
				fmt.Printf("Tasks: unavailable (%v)\n", err)
				fmt.Printf("Agents: unavailable (%v)\n", err)
			}
		}

		sessions, err := tmux.ListSessions()
		if err != nil {
			return err
		}
		sessionByWorkspace := make(map[string]tmux.SessionInfo, len(sessions))
		for _, s := range sessions {
			sessionByWorkspace[s.Workspace] = s
		}

		fmt.Printf("\nWorkspaces: %d active", len(sessions))
		if daemonRunning && len(workspaceInfos) > 0 {
			fmt.Printf(" (%d agent connection(s) online)", len(workspaceInfos))
		}
		fmt.Print("\n\n")

		// Try to render as a config tree, then list any unregistered sessions
		cfgPath, cfgErr := resolveConfigPath()
		if cfgErr == nil {
			if tree, err := config.LoadTree(cfgPath); err == nil && tree != nil {
				reconfigureEnabled := false
				if topology, err := loadTeamReconfigureTopology(cfgPath); err == nil {
					reconfigureEnabled = topology.Enabled
				}
				known := make(map[string]bool)
				collectKnownWorkspaces(tree, known)
				if reconfigureEnabled {
					fmt.Printf("Reconfigure: desired-only entries are configured but not running; runtime-only entries are outside %s\n\n", cfgPath)
				}
				printProjectTree(tree, 0, sessionByWorkspace, workspaceInfos, reconfigureEnabled)

				// Any running sessions not in the tree
				var unregistered []tmux.SessionInfo
				for _, s := range sessions {
					if !known[s.Workspace] {
						unregistered = append(unregistered, s)
					}
				}
				if len(unregistered) > 0 {
					fmt.Printf("\n%s\n", runtimeOnlyGroupLabel(reconfigureEnabled))
					for _, s := range unregistered {
						status := "detached"
						if s.Attached {
							status = "attached"
						}
						agentStatus := workspaceAgentStatus(workspaceInfos, s.Workspace)
						statusText := workspaceStatusPreview(workspaceInfos, s.Workspace, 72)
						fmt.Printf("  ● %-26s %-10s %-8s %s", s.Workspace, status, agentStatus, s.Name)
						if statusText != "" {
							fmt.Printf(" | %s", statusText)
						}
						fmt.Println()
					}
					if reconfigureEnabled {
						fmt.Println("\nReview runtime-only leftovers before treating the reconfiguration as reconciled.")
					} else {
						fmt.Println("\nRun 'ax init' in the project directory to register these.")
					}
				}
				return nil
			}
		}

		// Fallback: flat list
		if len(sessions) > 0 {
			fmt.Printf("%-24s %-10s %-8s %-18s %s\n", "NAME", "TMUX", "AGENT", "SESSION", "STATUS TEXT")
			fmt.Printf("%-24s %-10s %-8s %-18s %s\n", "----", "----", "-----", "-------", "-----------")
			for _, s := range sessions {
				status := "detached"
				if s.Attached {
					status = "attached"
				}
				fmt.Printf("%-24s %-10s %-8s %-18s %s\n",
					s.Workspace,
					status,
					workspaceAgentStatus(workspaceInfos, s.Workspace),
					s.Name,
					workspaceStatusPreview(workspaceInfos, s.Workspace, 64),
				)
			}
		}
		return nil
	},
}

func collectKnownWorkspaces(node *config.ProjectNode, known map[string]bool) {
	if node == nil {
		return
	}
	if rootOrchestratorVisible(node) {
		orchName := "orchestrator"
		if node.Prefix != "" {
			orchName = node.Prefix + ".orchestrator"
		}
		known[orchName] = true
	}
	for _, ws := range node.Workspaces {
		known[ws.MergedName] = true
	}
	for _, child := range node.Children {
		collectKnownWorkspaces(child, known)
	}
}

func printProjectTree(node *config.ProjectNode, level int, sessionByWorkspace map[string]tmux.SessionInfo, workspaceInfos map[string]types.WorkspaceInfo, reconfigureEnabled bool) {
	if node == nil {
		return
	}
	indent := strings.Repeat("  ", level)
	fmt.Printf("%s▾ %s\n", indent, node.DisplayName())

	if rootOrchestratorVisible(node) {
		orchName := "orchestrator"
		if node.Prefix != "" {
			orchName = node.Prefix + ".orchestrator"
		}
		allowDesired := !(node.Prefix == "" && orchName == "orchestrator")
		printLeaf(level+1, "◆ orchestrator", orchName, sessionByWorkspace, workspaceInfos, reconfigureEnabled && allowDesired)
	}

	for _, ws := range node.Workspaces {
		printLeaf(level+1, ws.Name, ws.MergedName, sessionByWorkspace, workspaceInfos, reconfigureEnabled)
	}

	for _, child := range node.Children {
		printProjectTree(child, level+1, sessionByWorkspace, workspaceInfos, reconfigureEnabled)
	}
}

func printLeaf(level int, label, mergedName string, sessionByWorkspace map[string]tmux.SessionInfo, workspaceInfos map[string]types.WorkspaceInfo, reconfigureEnabled bool) {
	indent := strings.Repeat("  ", level)
	agentStatus := workspaceAgentStatus(workspaceInfos, mergedName)
	statusText := workspaceStatusPreview(workspaceInfos, mergedName, 72)
	if s, ok := sessionByWorkspace[mergedName]; ok {
		status := "detached"
		if s.Attached {
			status = "attached"
		}
		fmt.Printf("%s● %-26s %-10s %-8s %s", indent, label, status, agentStatus, s.Name)
		if statusText != "" {
			fmt.Printf(" | %s", statusText)
		}
		fmt.Println()
	} else {
		tmuxStatus := reconfigureStatusTmuxState(agentStatus, reconfigureEnabled)
		fmt.Printf("%s○ %-26s %-10s %-8s", indent, label, tmuxStatus, agentStatus)
		if statusText != "" {
			fmt.Printf(" %s", statusText)
		}
		fmt.Println()
	}
}

func init() {
	rootCmd.AddCommand(statusCmd)
}
