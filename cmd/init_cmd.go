package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/ashon/ax/internal/config"
	"github.com/spf13/cobra"
)

var initGlobal bool

var initCmd = &cobra.Command{
	Use:   "init",
	Short: "Initialize .ax/config.yaml in the current directory or globally",
	RunE: func(cmd *cobra.Command, args []string) error {
		var dir, path, projectName string

		if initGlobal {
			home, err := os.UserHomeDir()
			if err != nil {
				return fmt.Errorf("resolve home directory: %w", err)
			}
			dir = home
			path = config.DefaultConfigPath(dir)
			projectName = "global"
		} else {
			dir = mustGetwd()
			path = config.DefaultConfigPath(dir)
			projectName = filepath.Base(dir)
		}

		if _, err := os.Stat(path); err == nil {
			return fmt.Errorf("%s already exists", path)
		}
		if !initGlobal {
			if legacyPath, ok := configPathConflict(dir); ok {
				return fmt.Errorf("legacy config already exists at %s", legacyPath)
			}
		}

		cfg := config.DefaultConfig(projectName)

		if err := cfg.Save(path); err != nil {
			return err
		}

		fmt.Printf("Created %s\n", path)
		fmt.Println("Edit it to define your workspaces, then run: ax up")
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

func configPathConflict(dir string) (string, bool) {
	if path, ok := configPathExists(config.DefaultConfigPath(dir)); ok {
		return path, true
	}
	if path, ok := configPathExists(config.LegacyConfigPath(dir)); ok {
		return path, true
	}
	return "", false
}

func configPathExists(path string) (string, bool) {
	if _, err := os.Stat(path); err == nil {
		return path, true
	}
	return "", false
}

func init() {
	initCmd.Flags().BoolVarP(&initGlobal, "global", "g", false, "initialize global config at ~/.ax/config.yaml")
	rootCmd.AddCommand(initCmd)
}
