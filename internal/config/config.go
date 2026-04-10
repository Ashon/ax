package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"gopkg.in/yaml.v3"
)

const DefaultConfigFile = "amux.yaml"

type Config struct {
	Project    string                `yaml:"project"`
	Workspaces map[string]Workspace `yaml:"workspaces"`
}

type Workspace struct {
	Dir         string            `yaml:"dir"`
	Description string            `yaml:"description,omitempty"`
	Shell       string            `yaml:"shell,omitempty"`
	Agent        string            `yaml:"agent,omitempty"`        // command to auto-start (default: "claude --dangerously-skip-permissions")
	Instructions string            `yaml:"instructions,omitempty"` // agent instructions (written to CLAUDE.md)
	Env          map[string]string `yaml:"env,omitempty"`
}

func Load(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read config: %w", err)
	}

	var cfg Config
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parse config: %w", err)
	}

	if cfg.Project == "" {
		cfg.Project = filepath.Base(filepath.Dir(path))
	}

	// Resolve dirs: expand ~ and resolve relative paths
	configDir := filepath.Dir(path)
	home, _ := os.UserHomeDir()
	for name, ws := range cfg.Workspaces {
		if ws.Dir == "" {
			ws.Dir = "."
		}
		if strings.HasPrefix(ws.Dir, "~/") {
			ws.Dir = filepath.Join(home, ws.Dir[2:])
		} else if !filepath.IsAbs(ws.Dir) {
			ws.Dir = filepath.Join(configDir, ws.Dir)
		}
		cfg.Workspaces[name] = ws
	}

	return &cfg, nil
}

func FindConfigFile() (string, error) {
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}

	for {
		path := filepath.Join(dir, DefaultConfigFile)
		if _, err := os.Stat(path); err == nil {
			return path, nil
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}

	return "", fmt.Errorf("%s not found (searched from current directory upward)", DefaultConfigFile)
}

func DefaultConfig(projectName string) *Config {
	return &Config{
		Project: projectName,
		Workspaces: map[string]Workspace{
			"main": {
				Dir:         ".",
				Description: "Main workspace",
			},
		},
	}
}

func (c *Config) Save(path string) error {
	data, err := yaml.Marshal(c)
	if err != nil {
		return fmt.Errorf("marshal config: %w", err)
	}
	return os.WriteFile(path, data, 0o644)
}
