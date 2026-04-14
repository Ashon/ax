package config

import (
	"fmt"
	"path/filepath"
	"sort"
)

// ProjectNode represents one project in a potentially-nested ax hierarchy.
// Each node knows its own workspaces and its child projects so callers
// can render a tree without flattening workspace names.
type ProjectNode struct {
	Name                    string // actual project name from the child config itself
	Alias                   string // mount alias used by the parent children mapping
	Prefix                  string // fully-qualified prefix used for merged names
	Dir                     string
	OrchestratorRuntime     string
	DisableRootOrchestrator bool
	Workspaces              []WorkspaceRef
	Children                []*ProjectNode
}

// WorkspaceRef is a workspace belonging to a project, with the merged
// "prefix.workspace" name that identifies its tmux session at runtime.
type WorkspaceRef struct {
	Name         string // original workspace name inside its project
	MergedName   string // fully-qualified name used by the daemon/tmux
	Runtime      string
	Description  string
	Instructions string
}

func (n *ProjectNode) DisplayName() string {
	if n == nil {
		return ""
	}
	if n.Alias == "" || n.Alias == n.Name {
		return n.Name
	}
	return fmt.Sprintf("%s (%s)", n.Alias, n.Name)
}

// LoadTree reads a config and recursively walks its children to produce a
// project tree. Unlike Load, it preserves the hierarchy instead of merging
// child workspaces into the parent's map.
func LoadTree(path string) (*ProjectNode, error) {
	if err := validateConfigTree(path); err != nil {
		return nil, err
	}
	seen := make(map[string]bool)
	return loadTreeRecursive(path, "", seen)
}

func loadTreeRecursive(path, prefix string, seen map[string]bool) (*ProjectNode, error) {
	absPath, err := filepath.Abs(path)
	if err != nil {
		return nil, fmt.Errorf("resolve config path: %w", err)
	}
	if seen[absPath] {
		return nil, fmt.Errorf("%w at %s", ErrCyclicChildren, absPath)
	}
	seen[absPath] = true
	defer delete(seen, absPath)

	cfg, err := loadLocalConfig(absPath)
	if err != nil {
		return nil, err
	}

	node := &ProjectNode{
		Name:                    cfg.Project,
		Prefix:                  prefix,
		Dir:                     ConfigRootDir(absPath),
		OrchestratorRuntime:     cfg.OrchestratorRuntime,
		DisableRootOrchestrator: prefix == "" && cfg.DisableRootOrchestrator,
	}

	// Workspaces defined directly in this project
	wsNames := make([]string, 0, len(cfg.Workspaces))
	for name := range cfg.Workspaces {
		wsNames = append(wsNames, name)
	}
	sort.Strings(wsNames)
	for _, name := range wsNames {
		ws := cfg.Workspaces[name]
		merged := qualifyName(prefix, name)
		node.Workspaces = append(node.Workspaces, WorkspaceRef{
			Name:         name,
			MergedName:   merged,
			Runtime:      ws.Runtime,
			Description:  ws.Description,
			Instructions: ws.Instructions,
		})
	}

	// Child projects
	childNames := make([]string, 0, len(cfg.Children))
	for name := range cfg.Children {
		childNames = append(childNames, name)
	}
	sort.Strings(childNames)
	for _, name := range childNames {
		child := cfg.Children[name]
		childPrefix := qualifyName(prefix, child.Prefix)

		childPath, err := ConfigPathInDir(child.Dir)
		if err != nil {
			continue
		}
		childNode, err := loadTreeRecursive(childPath, childPrefix, seen)
		if err != nil {
			if isStaleMissingChildError(err) {
				continue
			}
			return nil, wrapChildLoadError(name, child.Dir, err)
		}
		if childNode == nil {
			continue
		}
		childNode.Alias = name
		node.Children = append(node.Children, childNode)
	}

	return node, nil
}
