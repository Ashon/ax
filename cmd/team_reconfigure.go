package cmd

import (
	"strings"

	"github.com/ashon/ax/internal/config"
)

type teamReconfigureTopology struct {
	Enabled    bool
	ConfigPath string
	Desired    map[string]bool
}

func loadTeamReconfigureTopology(cfgPath string) (teamReconfigureTopology, error) {
	enabled, err := experimentalMCPTeamReconfigureEnabled(cfgPath)
	if err != nil || !enabled {
		return teamReconfigureTopology{Enabled: false, ConfigPath: cfgPath}, err
	}

	tree, err := config.LoadTree(cfgPath)
	if err != nil {
		return teamReconfigureTopology{}, err
	}

	desired := make(map[string]bool)
	collectDesiredWorkspaceNames(tree, desired)
	return teamReconfigureTopology{
		Enabled:    true,
		ConfigPath: cfgPath,
		Desired:    desired,
	}, nil
}

func collectDesiredWorkspaceNames(node *config.ProjectNode, desired map[string]bool) {
	if node == nil {
		return
	}
	if rootOrchestratorVisible(node) {
		desired[configOrchestratorName(node.Prefix)] = true
	}
	for _, ws := range node.Workspaces {
		desired[ws.MergedName] = true
	}
	for _, child := range node.Children {
		collectDesiredWorkspaceNames(child, desired)
	}
}

func configOrchestratorName(prefix string) string {
	if prefix == "" {
		return "orchestrator"
	}
	return prefix + ".orchestrator"
}

func reconfigureRowState(name string, desired map[string]bool, hasSession, hasAgent bool) string {
	if len(desired) == 0 {
		return ""
	}
	switch {
	case desired[name] && !hasSession && !hasAgent:
		return "desired-only"
	case !desired[name] && (hasSession || hasAgent) && !strings.HasPrefix(name, "_"):
		return "runtime-only"
	default:
		return "configured"
	}
}

func reconfigureSidebarState(name string, desired map[string]bool, hasSession, hasAgent bool) string {
	switch reconfigureRowState(name, desired, hasSession, hasAgent) {
	case "desired-only":
		return "desired"
	case "runtime-only":
		return "runtime-only"
	default:
		return ""
	}
}

func reconfigureStatusTmuxState(agentStatus string, enabled bool) string {
	if enabled && agentStatus == "offline" {
		return "desired"
	}
	if agentStatus != "offline" {
		return "no-session"
	}
	return "offline"
}

func runtimeOnlyGroupLabel(enabled bool) string {
	if enabled {
		return "▾ runtime-only (not in config tree)"
	}
	return "▾ unregistered (not in config tree)"
}
