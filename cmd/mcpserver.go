package cmd

import (
	"github.com/ashon/amux/internal/mcpserver"
	"github.com/spf13/cobra"
)

var mcpWorkspace string

var mcpServerCmd = &cobra.Command{
	Use:    "mcp-server",
	Short:  "Run MCP server for a workspace (spawned by Claude Code)",
	Hidden: true,
	RunE: func(cmd *cobra.Command, args []string) error {
		if mcpWorkspace == "" {
			return cmd.Help()
		}
		return mcpserver.Run(mcpWorkspace, socketPath, configPath)
	},
}

func init() {
	mcpServerCmd.Flags().StringVar(&mcpWorkspace, "workspace", "", "workspace name (required)")
	mcpServerCmd.MarkFlagRequired("workspace")
	rootCmd.AddCommand(mcpServerCmd)
}
