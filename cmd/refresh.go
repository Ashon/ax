package cmd

import (
	"fmt"
	"os"
	"sort"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/workspace"
	"github.com/spf13/cobra"
)

var (
	refreshRestart      bool
	refreshStartMissing bool
)

var refreshCmd = &cobra.Command{
	Use:   "refresh",
	Short: "Refresh generated ax files and optionally reconcile sessions",
	Long:  "Rewrites workspace/orchestrator MCP config and instruction files so they match the current ax config. Optionally starts missing sessions or restarts running ones.",
	RunE: func(cmd *cobra.Command, args []string) error {
		cfgPath, err := resolveConfigPath()
		if err != nil {
			return err
		}
		cfg, err := config.Load(cfgPath)
		if err != nil {
			return err
		}
		tree, err := config.LoadTree(cfgPath)
		if err != nil {
			return fmt.Errorf("load config tree: %w", err)
		}

		sp := daemon.ExpandSocketPath(socketPath)
		daemonRunning := isDaemonRunning(sp)

		fmt.Printf("Config: %s\n", cfgPath)
		if daemonRunning {
			fmt.Println("Daemon: running")
		} else {
			fmt.Println("Daemon: stopped")
		}

		experimentalReconcile, err := experimentalMCPTeamReconfigureEnabled(cfgPath)
		if err != nil {
			return err
		}
		if experimentalReconcile {
			skipRoot, err := reconcileRootOrchestratorState(cfgPath)
			if err != nil {
				return err
			}
			desired, err := workspace.BuildDesiredState(cfg, tree, socketPath, cfgPath, !skipRoot)
			if err != nil {
				return err
			}
			reconciler := workspace.NewReconciler(socketPath, cfgPath)
			report, err := reconciler.ReconcileDesiredState(desired, workspace.ReconcileOptions{
				DaemonRunning:          daemonRunning,
				AllowDisruptiveChanges: true,
			})
			if err != nil {
				return err
			}
			printExperimentalReconcileReport(report)
			return nil
		}

		mgr := workspace.NewManager(socketPath, cfgPath)
		names := make([]string, 0, len(cfg.Workspaces))
		for name := range cfg.Workspaces {
			names = append(names, name)
		}
		sort.Strings(names)

		fmt.Println("\nWorkspaces:")
		for _, name := range names {
			ws := cfg.Workspaces[name]
			if err := workspace.EnsureArtifacts(name, ws, socketPath, cfgPath); err != nil {
				return fmt.Errorf("refresh workspace %q: %w", name, err)
			}

			switch {
			case refreshRestart && tmux.SessionExists(name):
				if err := mgr.Destroy(name, ws.Dir); err != nil {
					return fmt.Errorf("restart workspace %q: %w", name, err)
				}
				if err := mgr.Create(name, ws); err != nil {
					return fmt.Errorf("restart workspace %q: %w", name, err)
				}
				fmt.Printf("  %s: artifacts refreshed, session restarted\n", name)
			case refreshStartMissing && daemonRunning && !tmux.SessionExists(name):
				if err := mgr.Create(name, ws); err != nil {
					return fmt.Errorf("start workspace %q: %w", name, err)
				}
				fmt.Printf("  %s: artifacts refreshed, session started\n", name)
			case tmux.SessionExists(name):
				fmt.Printf("  %s: artifacts refreshed, session unchanged\n", name)
			default:
				fmt.Printf("  %s: artifacts refreshed, session offline\n", name)
			}
		}

		fmt.Println("\nOrchestrators:")
		if refreshRestart {
			destroyOrchestrators(tree)
			if daemonRunning {
				if err := ensureOrchestrators(tree, sp, cfgPath); err != nil {
					return err
				}
			} else if err := refreshOrchestratorArtifacts(tree, "", sp, cfgPath); err != nil {
				return err
			}
			fmt.Println("  tree: artifacts refreshed, running orchestrators restarted")
		} else if refreshStartMissing && daemonRunning {
			if err := ensureOrchestrators(tree, sp, cfgPath); err != nil {
				return err
			}
			fmt.Println("  tree: artifacts refreshed, missing orchestrators started")
		} else {
			if err := refreshOrchestratorArtifacts(tree, "", sp, cfgPath); err != nil {
				return err
			}
			fmt.Println("  tree: artifacts refreshed, sessions unchanged")
		}

		if !refreshRestart {
			fmt.Println("\nNote: running sessions keep their current agent process. Use --restart to apply runtime changes immediately.")
		}
		return nil
	},
}

func printExperimentalReconcileReport(report workspace.ReconcileReport) {
	fmt.Println("\nExperimental Runtime Reconcile:")
	if len(report.Actions) == 0 {
		fmt.Println("  no runtime/workspace/orchestrator changes")
	} else {
		for _, action := range report.Actions {
			line := fmt.Sprintf("  %s %s: %s", action.Kind, action.Name, action.Operation)
			if action.Details != "" {
				line += " (" + action.Details + ")"
			}
			fmt.Println(line)
		}
	}
	if report.RootManualRestartRequired {
		fmt.Println("\nNote: root foreground orchestrator requires manual relaunch to pick up artifact changes.")
	}
}

func refreshOrchestratorArtifacts(node *config.ProjectNode, parentName, socketPath, cfgPath string) error {
	skipRoot, err := reconcileRootOrchestratorState(cfgPath)
	if err != nil {
		return err
	}
	return refreshOrchestratorArtifactsNode(node, parentName, socketPath, cfgPath, skipRoot)
}

func refreshOrchestratorArtifactsNode(node *config.ProjectNode, parentName, socketPath, cfgPath string, skipRoot bool) error {
	if node == nil {
		return nil
	}
	selfName := workspace.OrchestratorName(node.Prefix)
	isRoot := node.Prefix == ""

	if !(isRoot && skipRoot) {
		orchDir, err := orchestratorDir(node)
		if err != nil {
			return err
		}
		if err := os.MkdirAll(orchDir, 0o755); err != nil {
			return err
		}
		if err := workspace.WriteMCPConfig(orchDir, selfName, socketPath, cfgPath); err != nil {
			return err
		}
		runtime := node.OrchestratorRuntime
		if runtime == "" {
			runtime = "claude"
		}
		if runtime == "codex" {
			if err := workspace.EnsureCodexConfig(orchDir, selfName, socketPath, cfgPath); err != nil {
				return err
			}
		}
		if err := workspace.WriteOrchestratorPrompt(orchDir, node, node.Prefix, parentName, runtime, socketPath); err != nil {
			return err
		}
	}

	childParentName := selfName
	if isRoot && skipRoot {
		childParentName = ""
	}
	for _, child := range node.Children {
		if err := refreshOrchestratorArtifactsNode(child, childParentName, socketPath, cfgPath, skipRoot); err != nil {
			return err
		}
	}
	return nil
}

func init() {
	refreshCmd.Flags().BoolVar(&refreshRestart, "restart", false, "restart running workspace and orchestrator sessions after refreshing artifacts")
	refreshCmd.Flags().BoolVar(&refreshStartMissing, "start-missing", false, "start configured sessions that are currently not running")
	rootCmd.AddCommand(refreshCmd)
}
