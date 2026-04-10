package workspace

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
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
	amuxBin, err := amuxBinaryPath()
	if err != nil {
		return fmt.Errorf("resolve amux binary: %w", err)
	}

	args := []string{"mcp-server", "--workspace", workspace, "--socket", socketPath}
	if configPath != "" {
		args = append(args, "--config", configPath)
	}

	cfg := mcpConfig{
		MCPServers: map[string]mcpServerEntry{
			"amux": {
				Command: amuxBin,
				Args:    args,
			},
		},
	}

	path := filepath.Join(dir, MCPConfigFile)

	// If .mcp.json already exists, merge our entry into it
	if existing, err := os.ReadFile(path); err == nil {
		var existingCfg mcpConfig
		if json.Unmarshal(existing, &existingCfg) == nil && existingCfg.MCPServers != nil {
			existingCfg.MCPServers["amux"] = cfg.MCPServers["amux"]
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

	delete(cfg.MCPServers, "amux")

	if len(cfg.MCPServers) == 0 {
		return os.Remove(path)
	}

	newData, _ := json.MarshalIndent(cfg, "", "  ")
	return os.WriteFile(path, append(newData, '\n'), 0o644)
}

func amuxBinaryPath() (string, error) {
	return os.Executable()
}
