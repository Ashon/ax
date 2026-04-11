package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/ashon/ax/internal/config"
	"github.com/spf13/cobra"
)

var version = "dev"

var socketPath string
var configPath string

var rootCmd = &cobra.Command{
	Use:     "ax",
	Short:   "Multi-agent LLM workspace manager built on tmux",
	Long:    "ax manages multiple LLM agent workspaces using tmux sessions and enables inter-agent communication via MCP.",
	Version: version,
}

func Execute() {
	if err := rootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func init() {
	rootCmd.PersistentFlags().StringVar(&socketPath, "socket", "~/.local/state/ax/daemon.sock", "daemon socket path")
	rootCmd.PersistentFlags().StringVar(&configPath, "config", "", "ax config path (default: search upward for .ax/config.yaml, then ax.yaml)")
}

func resolveConfigPath() (string, error) {
	if configPath == "" {
		return config.FindConfigFile()
	}
	return filepath.Abs(configPath)
}
