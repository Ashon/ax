package cmd

import (
	"fmt"
	"os"

	"gopkg.in/yaml.v3"
)

type experimentalFlags struct {
	ExperimentalMCPTeamReconfigure bool `yaml:"experimental_mcp_team_reconfigure,omitempty"`
}

func experimentalMCPTeamReconfigureEnabled(cfgPath string) (bool, error) {
	if cfgPath == "" {
		return false, nil
	}

	data, err := os.ReadFile(cfgPath)
	if err != nil {
		return false, fmt.Errorf("read config %s: %w", cfgPath, err)
	}

	var flags experimentalFlags
	if err := yaml.Unmarshal(data, &flags); err != nil {
		return false, fmt.Errorf("parse config %s: %w", cfgPath, err)
	}

	return flags.ExperimentalMCPTeamReconfigure, nil
}
