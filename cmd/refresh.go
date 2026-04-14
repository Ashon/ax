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
		mgr := workspace.NewManager(socketPath, cfgPath)

		fmt.Printf("Config: %s\n", cfgPath)
		if daemonRunning {
			fmt.Println("Daemon: running")
		} else {
			fmt.Println("Daemon: stopped")
		}

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

func refreshOrchestratorArtifacts(node *config.ProjectNode, parentName, socketPath, cfgPath string) error {
	if node == nil {
		return nil
	}
	orchDir, err := orchestratorDir(node)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(orchDir, 0o755); err != nil {
		return err
	}
	if err := workspace.WriteMCPConfig(orchDir, workspace.OrchestratorName(node.Prefix), socketPath, cfgPath); err != nil {
		return err
	}
	runtime := node.OrchestratorRuntime
	if runtime == "" {
		runtime = "claude"
	}
	if runtime == "codex" {
		if err := workspace.EnsureCodexConfig(orchDir, workspace.OrchestratorName(node.Prefix), socketPath, cfgPath); err != nil {
			return err
		}
	}
	if err := workspace.WriteOrchestratorPrompt(orchDir, node, node.Prefix, parentName, runtime); err != nil {
		return err
	}
	selfName := workspace.OrchestratorName(node.Prefix)
	for _, child := range node.Children {
		if err := refreshOrchestratorArtifacts(child, selfName, socketPath, cfgPath); err != nil {
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
