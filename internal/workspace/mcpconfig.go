package workspace

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/daemonutil"
)

const MCPConfigFile = ".mcp.json"

type mcpConfig struct {
	MCPServers map[string]mcpServerEntry `json:"mcpServers"`
}

type mcpServerEntry struct {
	Command string   `json:"command"`
	Args    []string `json:"args"`
}

func WriteMCPConfig(dir, workspace, socketPath, configPath string) error {
	axBin, err := axBinaryPath()
	if err != nil {
		return fmt.Errorf("resolve ax binary: %w", err)
	}

	args := []string{"mcp-server", "--workspace", workspace, "--socket", socketPath}
	if configPath != "" {
		args = append(args, "--config", configPath)
	}

	cfg := mcpConfig{
		MCPServers: map[string]mcpServerEntry{
			"ax": {
				Command: axBin,
				Args:    args,
			},
		},
	}

	path := filepath.Join(dir, MCPConfigFile)

	// If .mcp.json already exists, merge our entry into it
	if existing, err := os.ReadFile(path); err == nil {
		var existingCfg mcpConfig
		if json.Unmarshal(existing, &existingCfg) == nil && existingCfg.MCPServers != nil {
			existingCfg.MCPServers["ax"] = cfg.MCPServers["ax"]
			cfg = existingCfg
		}
	}

	data, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal mcp config: %w", err)
	}

	return os.WriteFile(path, append(data, '\n'), 0o644)
}

func RemoveMCPConfig(dir string) error {
	path := filepath.Join(dir, MCPConfigFile)

	data, err := os.ReadFile(path)
	if err != nil {
		return nil // no file, nothing to do
	}

	var cfg mcpConfig
	if err := json.Unmarshal(data, &cfg); err != nil {
		return nil
	}

	delete(cfg.MCPServers, "ax")

	if len(cfg.MCPServers) == 0 {
		return os.Remove(path)
	}

	newData, _ := json.MarshalIndent(cfg, "", "  ")
	return os.WriteFile(path, append(newData, '\n'), 0o644)
}

func EnsureCodexConfig(dir, workspace, socketPath, configPath string) error {
	axBin, err := axBinaryPath()
	if err != nil {
		return fmt.Errorf("resolve ax binary: %w", err)
	}
	if _, err := agent.PrepareCodexHome(workspace, dir, daemonutil.ExpandSocketPath(socketPath), axBin, configPath); err != nil {
		return fmt.Errorf("prepare codex home: %w", err)
	}
	return nil
}

func axBinaryPath() (string, error) {
	return os.Executable()
}
