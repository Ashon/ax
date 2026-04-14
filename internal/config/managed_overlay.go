package config

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"gopkg.in/yaml.v3"
)

const ManagedOverlayFile = "managed_overlay.yaml"

// ManagedOverlay is a machine-managed, persisted overlay merged with the
// user-authored base config when experimental MCP team reconfiguration is on.
type ManagedOverlay struct {
	Policies   ManagedPolicyOverlay             `yaml:"policies,omitempty"`
	Workspaces map[string]ManagedWorkspacePatch `yaml:"workspaces,omitempty"`
	Children   map[string]ManagedChildPatch     `yaml:"children,omitempty"`
}

type ManagedPolicyOverlay struct {
	OrchestratorRuntime     *string `yaml:"orchestrator_runtime,omitempty"`
	DisableRootOrchestrator *bool   `yaml:"disable_root_orchestrator,omitempty"`
}

type ManagedWorkspacePatch struct {
	Delete      bool    `yaml:"delete,omitempty"`
	Enabled     *bool   `yaml:"enabled,omitempty"`
	Dir         *string `yaml:"dir,omitempty"`
	Description *string `yaml:"description,omitempty"`
	Runtime     *string `yaml:"runtime,omitempty"`
	Shell       *string `yaml:"shell,omitempty"`
	Agent       *string `yaml:"agent,omitempty"`
}

type ManagedChildPatch struct {
	Delete  bool    `yaml:"delete,omitempty"`
	Enabled *bool   `yaml:"enabled,omitempty"`
	Dir     *string `yaml:"dir,omitempty"`
	Prefix  *string `yaml:"prefix,omitempty"`
}

func ManagedOverlayPath(configPath string) string {
	if absPath, err := filepath.Abs(configPath); err == nil {
		configPath = absPath
	}
	return filepath.Join(ConfigRootDir(configPath), DefaultConfigDir, ManagedOverlayFile)
}

func LoadManagedOverlay(configPath string) (*ManagedOverlay, error) {
	path := ManagedOverlayPath(configPath)
	data, err := os.ReadFile(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return &ManagedOverlay{}, nil
		}
		return nil, fmt.Errorf("read managed overlay %s: %w", path, err)
	}

	var overlay ManagedOverlay
	if err := yaml.Unmarshal(data, &overlay); err != nil {
		return nil, fmt.Errorf("parse managed overlay %s: %w", path, err)
	}
	return &overlay, nil
}

func (o *ManagedOverlay) Save(configPath string) error {
	if o == nil {
		o = &ManagedOverlay{}
	}

	data, err := yaml.Marshal(o)
	if err != nil {
		return fmt.Errorf("marshal managed overlay: %w", err)
	}

	path := ManagedOverlayPath(configPath)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create managed overlay dir: %w", err)
	}
	return os.WriteFile(path, data, 0o644)
}

func loadLocalConfig(path string) (Config, error) {
	cfg, err := readConfigFile(path)
	if err != nil {
		return Config{}, err
	}

	initializeLocalConfig(&cfg, path)
	if cfg.ExperimentalMCPTeamReconfigure {
		overlay, err := LoadManagedOverlay(path)
		if err != nil {
			return Config{}, err
		}
		applyManagedOverlay(&cfg, overlay)
	}
	normalizeLocalConfig(&cfg, path)

	return cfg, nil
}

func readConfigFile(path string) (Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Config{}, fmt.Errorf("read config %s: %w", path, err)
	}

	var cfg Config
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return Config{}, fmt.Errorf("parse config %s: %w", path, err)
	}
	return cfg, nil
}

func initializeLocalConfig(cfg *Config, path string) {
	if cfg.Project == "" {
		cfg.Project = filepath.Base(ConfigRootDir(path))
	}
	if cfg.Workspaces == nil {
		cfg.Workspaces = make(map[string]Workspace)
	}
	if cfg.Children == nil {
		cfg.Children = make(map[string]Child)
	}
}

func normalizeLocalConfig(cfg *Config, path string) {
	projectDir := ConfigRootDir(path)

	for name, ws := range cfg.Workspaces {
		ws.Dir = resolveDir(projectDir, ws.Dir)
		if strings.TrimSpace(ws.CodexModelReasoningEffort) == "" {
			ws.CodexModelReasoningEffort = strings.TrimSpace(cfg.CodexModelReasoningEffort)
		}
		cfg.Workspaces[name] = ws
	}

	for name, child := range cfg.Children {
		child.Dir = resolveDir(projectDir, child.Dir)
		if child.Prefix == "" {
			child.Prefix = name
		}
		cfg.Children[name] = child
	}
}

func applyManagedOverlay(cfg *Config, overlay *ManagedOverlay) {
	if cfg.Workspaces == nil {
		cfg.Workspaces = make(map[string]Workspace)
	}
	if cfg.Children == nil {
		cfg.Children = make(map[string]Child)
	}
	if overlay == nil {
		return
	}

	if overlay.Policies.OrchestratorRuntime != nil {
		cfg.OrchestratorRuntime = *overlay.Policies.OrchestratorRuntime
	}
	if overlay.Policies.DisableRootOrchestrator != nil {
		cfg.DisableRootOrchestrator = *overlay.Policies.DisableRootOrchestrator
	}

	for name, patch := range overlay.Workspaces {
		if patch.Delete || isDisabled(patch.Enabled) {
			delete(cfg.Workspaces, name)
			continue
		}

		ws := cfg.Workspaces[name]
		if patch.Dir != nil {
			ws.Dir = *patch.Dir
		}
		if patch.Description != nil {
			ws.Description = *patch.Description
		}
		if patch.Runtime != nil {
			ws.Runtime = *patch.Runtime
		}
		if patch.Shell != nil {
			ws.Shell = *patch.Shell
		}
		if patch.Agent != nil {
			ws.Agent = *patch.Agent
		}
		cfg.Workspaces[name] = ws
	}

	for name, patch := range overlay.Children {
		if patch.Delete || isDisabled(patch.Enabled) {
			delete(cfg.Children, name)
			continue
		}

		child := cfg.Children[name]
		if patch.Dir != nil {
			child.Dir = *patch.Dir
		}
		if patch.Prefix != nil {
			child.Prefix = *patch.Prefix
		}
		cfg.Children[name] = child
	}
}

func isDisabled(enabled *bool) bool {
	return enabled != nil && !*enabled
}
