package cmd

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
)

var socketPath string

var rootCmd = &cobra.Command{
	Use:   "amux",
	Short: "Multi-agent LLM workspace manager built on tmux",
	Long:  "amux manages multiple LLM agent workspaces using tmux sessions and enables inter-agent communication via MCP.",
}

func Execute() {
	if err := rootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func init() {
	rootCmd.PersistentFlags().StringVar(&socketPath, "socket", "~/.local/state/amux/daemon.sock", "daemon socket path")
}
