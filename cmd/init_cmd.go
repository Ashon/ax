package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/ashon/amux/internal/config"
	"github.com/spf13/cobra"
)

var initCmd = &cobra.Command{
	Use:   "init",
	Short: "Initialize amux.yaml in the current directory",
	RunE: func(cmd *cobra.Command, args []string) error {
		path := filepath.Join(".", config.DefaultConfigFile)
		if _, err := os.Stat(path); err == nil {
			return fmt.Errorf("%s already exists", path)
		}

		projectName := filepath.Base(mustGetwd())
		cfg := config.DefaultConfig(projectName)

		if err := cfg.Save(path); err != nil {
			return err
		}

		fmt.Printf("Created %s\n", path)
		fmt.Println("Edit it to define your workspaces, then run: amux up")
		return nil
	},
}

func mustGetwd() string {
	dir, err := os.Getwd()
	if err != nil {
		return "."
	}
	return dir
}

func init() {
	rootCmd.AddCommand(initCmd)
}
