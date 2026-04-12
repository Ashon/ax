package config

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"
)

const (
	DefaultConfigDir  = ".ax"
	DefaultConfigFile = "config.yaml"
	LegacyConfigFile  = "ax.yaml"
)

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

var ErrCyclicChildren = fmt.Errorf("cyclic ax children reference")

func loadRecursive(path string, seen map[string]bool) (*Config, error) {
	path, err := filepath.Abs(path)
	if err != nil {
		return nil, fmt.Errorf("resolve config path: %w", err)
	}
	if seen[path] {
		return nil, fmt.Errorf("%w at %s", ErrCyclicChildren, path)
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
	projectDir := configBaseDir(configDir)
	for name, ws := range cfg.Workspaces {
		ws.Dir = resolveDir(projectDir, ws.Dir)
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
		child.Dir = resolveDir(projectDir, child.Dir)
		if child.Dir == "" {
			fmt.Fprintf(os.Stderr, "warning: child %q is missing dir, skipping\n", name)
			continue
		}
		if child.Prefix == "" {
			child.Prefix = name
		}
		cfg.Children[name] = child

		childCfgPath, err := ConfigPathInDir(child.Dir)
		if err != nil {
			// Stale entry — config file no longer exists at that path.
			// Skip so the rest of the tree still loads.
			fmt.Fprintf(os.Stderr, "warning: child %q at %s has no config, skipping\n", name, child.Dir)
			continue
		}
		childCfg, err := loadRecursive(childCfgPath, seen)
		if err != nil {
			// Cycles are fatal; other errors are degraded to warnings.
			if errors.Is(err, ErrCyclicChildren) {
				return nil, err
			}
			fmt.Fprintf(os.Stderr, "warning: failed to load child %q: %v\n", name, err)
			continue
		}

		for childName, ws := range childCfg.Workspaces {
			mergedName := child.Prefix + "." + childName
			if _, exists := merged.Workspaces[mergedName]; exists {
				fmt.Fprintf(os.Stderr, "warning: duplicate workspace %q from child %q, skipping\n", mergedName, name)
				continue
			}
			merged.Workspaces[mergedName] = ws
		}
	}

	return merged, nil
}

// FindConfigFile walks upward from the current directory and returns the
// topmost ancestor .ax/config.yaml it finds. Also checks the user's home
// directory so a global config always acts as the tree root when present.
func FindConfigFile() (string, error) {
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}

	var topMost string
	for {
		if path, ok := findConfigInDir(dir); ok {
			topMost = path
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}

	if home, err := os.UserHomeDir(); err == nil {
		if path, ok := findConfigInDir(home); ok {
			topMost = path
		}
	}

	if topMost != "" {
		return topMost, nil
	}
	return "", fmt.Errorf(".ax/config.yaml or %s not found (searched from current directory upward)", LegacyConfigFile)
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

func configBaseDir(configDir string) string {
	if filepath.Base(configDir) == DefaultConfigDir {
		return filepath.Dir(configDir)
	}
	return configDir
}

func DefaultConfigPath(dir string) string {
	return filepath.Join(dir, DefaultConfigDir, DefaultConfigFile)
}

func LegacyConfigPath(dir string) string {
	return filepath.Join(dir, LegacyConfigFile)
}

func ConfigPathInDir(dir string) (string, error) {
	if path, ok := findConfigInDir(dir); ok {
		return path, nil
	}
	return "", fmt.Errorf(".ax/config.yaml or %s not found in %s", LegacyConfigFile, dir)
}

func ConfigRootDir(path string) string {
	return configBaseDir(filepath.Dir(path))
}

func findConfigInDir(dir string) (string, bool) {
	preferred := DefaultConfigPath(dir)
	if _, err := os.Stat(preferred); err == nil {
		return preferred, true
	}

	legacy := LegacyConfigPath(dir)
	if _, err := os.Stat(legacy); err == nil {
		return legacy, true
	}

	return "", false
}

func (c *Config) Save(path string) error {
	data, err := yaml.Marshal(c)
	if err != nil {
		return fmt.Errorf("marshal config: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create config dir: %w", err)
	}
	return os.WriteFile(path, data, 0o644)
}
