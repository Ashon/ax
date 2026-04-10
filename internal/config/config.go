package config

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"
)

const DefaultConfigFile = "amux.yaml"

type Config struct {
	Project             string               `yaml:"project"`
	OrchestratorRuntime string               `yaml:"orchestrator_runtime,omitempty"`
	Children            map[string]Child     `yaml:"children,omitempty"`
	Workspaces          map[string]Workspace `yaml:"workspaces"`
}

type Child struct {
	Dir    string `yaml:"dir"`
	Prefix string `yaml:"prefix,omitempty"`
}

type Workspace struct {
	Dir          string            `yaml:"dir"`
	Description  string            `yaml:"description,omitempty"`
	Shell        string            `yaml:"shell,omitempty"`
	Runtime      string            `yaml:"runtime,omitempty"`      // claude or codex (default: claude)
	Agent        string            `yaml:"agent,omitempty"`        // custom command to auto-start instead of runtime default
	Instructions string            `yaml:"instructions,omitempty"` // agent instructions (written to the runtime's instruction file)
	Env          map[string]string `yaml:"env,omitempty"`
}

func Load(path string) (*Config, error) {
	seen := make(map[string]bool)
	return loadRecursive(path, seen)
}

func loadRecursive(path string, seen map[string]bool) (*Config, error) {
	path, err := filepath.Abs(path)
	if err != nil {
		return nil, fmt.Errorf("resolve config path: %w", err)
	}
	if seen[path] {
		return nil, fmt.Errorf("cyclic amux children reference detected at %s", path)
	}
	seen[path] = true
	defer delete(seen, path)

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
	if cfg.Workspaces == nil {
		cfg.Workspaces = make(map[string]Workspace)
	}

	configDir := filepath.Dir(path)
	for name, ws := range cfg.Workspaces {
		ws.Dir = resolveDir(configDir, ws.Dir)
		cfg.Workspaces[name] = ws
	}

	merged := &Config{
		Project:             cfg.Project,
		OrchestratorRuntime: cfg.OrchestratorRuntime,
		Children:            cfg.Children,
		Workspaces:          make(map[string]Workspace, len(cfg.Workspaces)),
	}
	for name, ws := range cfg.Workspaces {
		merged.Workspaces[name] = ws
	}

	childNames := make([]string, 0, len(cfg.Children))
	for name := range cfg.Children {
		childNames = append(childNames, name)
	}
	sort.Strings(childNames)

	for _, name := range childNames {
		child := cfg.Children[name]
		child.Dir = resolveDir(configDir, child.Dir)
		if child.Dir == "" {
			return nil, fmt.Errorf("child %q is missing dir", name)
		}
		if child.Prefix == "" {
			child.Prefix = name
		}
		cfg.Children[name] = child

		childCfgPath := filepath.Join(child.Dir, DefaultConfigFile)
		childCfg, err := loadRecursive(childCfgPath, seen)
		if err != nil {
			return nil, fmt.Errorf("load child %q: %w", name, err)
		}

		for childName, ws := range childCfg.Workspaces {
			mergedName := child.Prefix + "." + childName
			if _, exists := merged.Workspaces[mergedName]; exists {
				return nil, fmt.Errorf("duplicate workspace name %q after importing child %q", mergedName, name)
			}
			merged.Workspaces[mergedName] = ws
		}
	}

	return merged, nil
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

func resolveDir(baseDir, value string) string {
	if value == "" {
		value = "."
	}

	home, _ := os.UserHomeDir()
	if strings.HasPrefix(value, "~/") {
		return filepath.Join(home, value[2:])
	}
	if filepath.IsAbs(value) {
		return value
	}
	return filepath.Join(baseDir, value)
}

func (c *Config) Save(path string) error {
	data, err := yaml.Marshal(c)
	if err != nil {
		return fmt.Errorf("marshal config: %w", err)
	}
	return os.WriteFile(path, data, 0o644)
}
