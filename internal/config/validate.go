package config

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"

	"gopkg.in/yaml.v3"
)

var ErrDuplicateChildPrefix = fmt.Errorf("duplicate ax child prefix")
var ErrReservedNameCollision = fmt.Errorf("reserved ax session name collision")

type validationState struct {
	childPrefixes map[string]childPrefixClaim
	workspaces    map[string]workspaceClaim
	orchestrators map[string]orchestratorClaim
}

type childPrefixClaim struct {
	ConfigPath string
	ChildName  string
	ChildDir   string
	Prefix     string
}

type workspaceClaim struct {
	ConfigPath string
	Name       string
	MergedName string
}

type orchestratorClaim struct {
	ConfigPath  string
	ChildName   string
	ChildDir    string
	Prefix      string
	SessionName string
}

func validateConfigTree(path string) error {
	absPath, err := filepath.Abs(path)
	if err != nil {
		return fmt.Errorf("resolve config path: %w", err)
	}

	rootCfg, err := readConfigForValidation(absPath)
	if err != nil {
		return err
	}

	orchestrators := make(map[string]orchestratorClaim)
	if !rootCfg.DisableRootOrchestrator {
		orchestrators[orchestratorSessionName("")] = orchestratorClaim{
			ConfigPath:  absPath,
			SessionName: orchestratorSessionName(""),
		}
	}

	state := &validationState{
		childPrefixes: make(map[string]childPrefixClaim),
		workspaces:    make(map[string]workspaceClaim),
		orchestrators: orchestrators,
	}

	seen := make(map[string]bool)
	return validateRecursive(absPath, "", seen, state)
}

func readConfigForValidation(path string) (Config, error) {
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

func validateRecursive(path, prefix string, seen map[string]bool, state *validationState) error {
	absPath, err := filepath.Abs(path)
	if err != nil {
		return fmt.Errorf("resolve config path: %w", err)
	}
	if seen[absPath] {
		return fmt.Errorf("%w at %s", ErrCyclicChildren, absPath)
	}
	seen[absPath] = true
	defer delete(seen, absPath)

	cfg, err := readConfigForValidation(absPath)
	if err != nil {
		return err
	}

	configDir := filepath.Dir(absPath)
	projectDir := configBaseDir(configDir)

	workspaceNames := make([]string, 0, len(cfg.Workspaces))
	for name := range cfg.Workspaces {
		workspaceNames = append(workspaceNames, name)
	}
	sort.Strings(workspaceNames)

	for _, name := range workspaceNames {
		mergedName := qualifyName(prefix, name)
		if existing, ok := state.orchestrators[mergedName]; ok {
			return reservedNameCollisionForWorkspace(absPath, name, mergedName, existing)
		}
		if _, exists := state.workspaces[mergedName]; !exists {
			state.workspaces[mergedName] = workspaceClaim{
				ConfigPath: absPath,
				Name:       name,
				MergedName: mergedName,
			}
		}
	}

	childNames := make([]string, 0, len(cfg.Children))
	for name := range cfg.Children {
		childNames = append(childNames, name)
	}
	sort.Strings(childNames)

	for _, name := range childNames {
		child := cfg.Children[name]
		childDir := resolveDir(projectDir, child.Dir)
		if child.Prefix == "" {
			child.Prefix = name
		}
		childCfgPath, err := ConfigPathInDir(childDir)
		if err != nil {
			continue
		}

		claim := childPrefixClaim{
			ConfigPath: absPath,
			ChildName:  name,
			ChildDir:   childDir,
			Prefix:     qualifyName(prefix, child.Prefix),
		}
		if existing, ok := state.childPrefixes[claim.Prefix]; ok {
			return duplicateChildPrefixError(existing, claim)
		}

		orchClaim := orchestratorClaim{
			ConfigPath:  absPath,
			ChildName:   name,
			ChildDir:    childDir,
			Prefix:      claim.Prefix,
			SessionName: orchestratorSessionName(claim.Prefix),
		}
		if existing, ok := state.workspaces[orchClaim.SessionName]; ok {
			return reservedNameCollisionForChild(orchClaim, existing)
		}

		state.childPrefixes[claim.Prefix] = claim
		state.orchestrators[orchClaim.SessionName] = orchClaim

		if err := validateRecursive(childCfgPath, claim.Prefix, seen, state); err != nil {
			if isStaleMissingChildError(err) {
				delete(state.childPrefixes, claim.Prefix)
				delete(state.orchestrators, orchClaim.SessionName)
				continue
			}
			return wrapChildLoadError(name, childDir, err)
		}
	}

	return nil
}

func qualifyName(prefix, name string) string {
	if prefix == "" {
		return name
	}
	return prefix + "." + name
}

func orchestratorSessionName(prefix string) string {
	if prefix == "" {
		return "orchestrator"
	}
	return prefix + ".orchestrator"
}

func duplicateChildPrefixError(first, second childPrefixClaim) error {
	return fmt.Errorf(
		"%w %q: child %q in %s -> %s conflicts with child %q in %s -> %s",
		ErrDuplicateChildPrefix,
		second.Prefix,
		first.ChildName,
		first.ConfigPath,
		first.ChildDir,
		second.ChildName,
		second.ConfigPath,
		second.ChildDir,
	)
}

func reservedNameCollisionForWorkspace(configPath, workspaceName, mergedName string, existing orchestratorClaim) error {
	return fmt.Errorf(
		"%w %q: workspace %q in %s conflicts with %s",
		ErrReservedNameCollision,
		mergedName,
		workspaceName,
		configPath,
		describeOrchestratorClaim(existing),
	)
}

func reservedNameCollisionForChild(child orchestratorClaim, existing workspaceClaim) error {
	return fmt.Errorf(
		"%w %q: child %q in %s -> %s (prefix %q) conflicts with workspace %q in %s",
		ErrReservedNameCollision,
		child.SessionName,
		child.ChildName,
		child.ConfigPath,
		child.ChildDir,
		child.Prefix,
		existing.Name,
		existing.ConfigPath,
	)
}

func describeOrchestratorClaim(claim orchestratorClaim) string {
	if claim.ChildName == "" {
		return fmt.Sprintf("the root orchestrator in %s", claim.ConfigPath)
	}
	return fmt.Sprintf(
		"child %q in %s -> %s (prefix %q, orchestrator %q)",
		claim.ChildName,
		claim.ConfigPath,
		claim.ChildDir,
		claim.Prefix,
		claim.SessionName,
	)
}
