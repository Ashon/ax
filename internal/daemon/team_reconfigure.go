package daemon

import (
	"crypto/sha1"
	"encoding/hex"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/types"
	"gopkg.in/yaml.v3"
)

const teamApplyLeaseTTL = 2 * time.Minute

type teamApplyLease struct {
	teamID        string
	token         string
	startedAt     time.Time
	expiry        time.Time
	reconcileMode types.TeamReconcileMode
}

type teamController struct {
	stateDir string
	store    *TeamStateStore

	mu     sync.Mutex
	leases map[string]teamApplyLease
}

func newTeamController(stateDir string, store *TeamStateStore) *teamController {
	return &teamController{
		stateDir: stateDir,
		store:    store,
		leases:   make(map[string]teamApplyLease),
	}
}

func (d *Daemon) getTeamState(configPath string) (types.TeamReconfigureState, error) {
	return d.teamController.getState(configPath, d.teamReconfigureEnabled())
}

func (d *Daemon) dryRunTeamReconfigure(configPath string, expectedRevision *int, changes []types.TeamReconfigureChange) (types.TeamReconfigurePlan, error) {
	if !d.teamReconfigureEnabled() {
		return types.TeamReconfigurePlan{}, fmt.Errorf("feature flag %q is disabled", types.ExperimentalMCPTeamReconfigureFlagKey)
	}
	return d.teamController.plan(configPath, expectedRevision, changes, d.teamReconfigureEnabled())
}

func (d *Daemon) beginTeamReconfigureApply(configPath string, expectedRevision *int, changes []types.TeamReconfigureChange, mode types.TeamReconcileMode) (types.TeamApplyTicket, error) {
	if !d.teamReconfigureEnabled() {
		return types.TeamApplyTicket{}, fmt.Errorf("feature flag %q is disabled", types.ExperimentalMCPTeamReconfigureFlagKey)
	}
	return d.teamController.beginApply(configPath, expectedRevision, changes, mode, d.teamReconfigureEnabled())
}

func (d *Daemon) finishTeamReconfigureApply(token string, success bool, errText string, actions []types.TeamReconfigureAction) (types.TeamReconfigureState, error) {
	return d.teamController.finishApply(token, success, errText, actions, d.teamReconfigureEnabled())
}

func (d *Daemon) teamReconfigureEnabled() bool {
	d.sharedMu.RLock()
	defer d.sharedMu.RUnlock()
	return parseBoolFlag(d.sharedValues[types.ExperimentalMCPTeamReconfigureFlagKey])
}

func parseBoolFlag(value string) bool {
	switch strings.TrimSpace(strings.ToLower(value)) {
	case "1", "true", "yes", "on", "enabled":
		return true
	default:
		return false
	}
}

func (c *teamController) getState(configPath string, featureEnabled bool) (types.TeamReconfigureState, error) {
	basePath, err := canonicalConfigPath(configPath)
	if err != nil {
		return types.TeamReconfigureState{}, err
	}
	state, err := c.currentState(basePath, featureEnabled)
	if err != nil {
		return types.TeamReconfigureState{}, err
	}
	return state, nil
}

func (c *teamController) plan(configPath string, expectedRevision *int, changes []types.TeamReconfigureChange, featureEnabled bool) (types.TeamReconfigurePlan, error) {
	basePath, err := canonicalConfigPath(configPath)
	if err != nil {
		return types.TeamReconfigurePlan{}, err
	}
	currentState, err := c.currentState(basePath, featureEnabled)
	if err != nil {
		return types.TeamReconfigurePlan{}, err
	}
	if expectedRevision != nil && currentState.Revision != *expectedRevision {
		return types.TeamReconfigurePlan{}, fmt.Errorf("team revision mismatch: expected %d, got %d", *expectedRevision, currentState.Revision)
	}
	currentCfgPath := currentState.EffectiveConfigPath
	if strings.TrimSpace(currentCfgPath) == "" {
		currentCfgPath = currentState.BaseConfigPath
	}
	currentCfg, err := config.Load(currentCfgPath)
	if err != nil {
		return types.TeamReconfigurePlan{}, err
	}
	currentTree, err := config.LoadTree(currentCfgPath)
	if err != nil {
		return types.TeamReconfigurePlan{}, err
	}

	nextOverlay, warnings, err := c.applyChanges(basePath, currentState.Overlay, changes)
	if err != nil {
		return types.TeamReconfigurePlan{}, err
	}
	nextState := currentState
	nextState.Revision = currentState.Revision + 1
	nextState.Overlay = nextOverlay
	effectivePath, desired, nextCfg, nextTree, err := c.materializeState(basePath, nextState, false)
	if err != nil {
		return types.TeamReconfigurePlan{}, err
	}
	nextState.EffectiveConfigPath = effectivePath
	nextState.Desired = desired
	nextState.FeatureEnabled = featureEnabled

	return types.TeamReconfigurePlan{
		State:            nextState,
		ExpectedRevision: currentState.Revision,
		Changes:          append([]types.TeamReconfigureChange(nil), changes...),
		Actions:          diffTeamActions(currentCfg, currentTree, nextCfg, nextTree),
		Warnings:         warnings,
	}, nil
}

func (c *teamController) beginApply(configPath string, expectedRevision *int, changes []types.TeamReconfigureChange, mode types.TeamReconcileMode, featureEnabled bool) (types.TeamApplyTicket, error) {
	if mode == "" {
		mode = types.TeamReconcileArtifactsOnly
	}
	switch mode {
	case types.TeamReconcileArtifactsOnly, types.TeamReconcileStartMissing:
	default:
		return types.TeamApplyTicket{}, fmt.Errorf("invalid reconcile mode %q", mode)
	}

	plan, err := c.plan(configPath, expectedRevision, changes, featureEnabled)
	if err != nil {
		return types.TeamApplyTicket{}, err
	}

	c.mu.Lock()
	defer c.mu.Unlock()

	now := time.Now()
	if lease, ok := c.leases[plan.State.TeamID]; ok && lease.expiry.After(now) {
		return types.TeamApplyTicket{}, fmt.Errorf("team reconfiguration already in progress for %s", plan.State.TeamID)
	}

	effectivePath, desired, _, _, err := c.materializeState(plan.State.BaseConfigPath, plan.State, true)
	if err != nil {
		return types.TeamApplyTicket{}, err
	}
	plan.State.EffectiveConfigPath = effectivePath
	plan.State.Desired = desired
	plan.State.FeatureEnabled = featureEnabled
	if err := c.store.Put(plan.State); err != nil {
		return types.TeamApplyTicket{}, err
	}

	token := newTicketToken(plan.State.TeamID, now)
	c.leases[plan.State.TeamID] = teamApplyLease{
		teamID:        plan.State.TeamID,
		token:         token,
		startedAt:     now,
		expiry:        now.Add(teamApplyLeaseTTL),
		reconcileMode: mode,
	}

	return types.TeamApplyTicket{
		Token:         token,
		Plan:          plan,
		ReconcileMode: mode,
	}, nil
}

func (c *teamController) finishApply(token string, success bool, errText string, actions []types.TeamReconfigureAction, featureEnabled bool) (types.TeamReconfigureState, error) {
	c.mu.Lock()
	defer c.mu.Unlock()

	var lease teamApplyLease
	found := false
	now := time.Now()
	for teamID, active := range c.leases {
		if active.token == token {
			lease = active
			delete(c.leases, teamID)
			found = true
			break
		}
	}
	if !found {
		return types.TeamReconfigureState{}, fmt.Errorf("team reconfigure token %q not found", token)
	}

	state, ok := c.store.Get(lease.teamID)
	if !ok {
		return types.TeamReconfigureState{}, fmt.Errorf("team state %q not found", lease.teamID)
	}
	state.FeatureEnabled = featureEnabled
	report := &types.TeamApplyReport{
		StartedAt:     lease.startedAt,
		FinishedAt:    &now,
		Success:       success,
		Error:         strings.TrimSpace(errText),
		ReconcileMode: lease.reconcileMode,
		Actions:       append([]types.TeamReconfigureAction(nil), actions...),
	}
	state.LastApply = report
	if err := c.store.Put(state); err != nil {
		return types.TeamReconfigureState{}, err
	}
	return state, nil
}

func (c *teamController) currentState(basePath string, featureEnabled bool) (types.TeamReconfigureState, error) {
	teamID := basePath
	if state, ok := c.store.Get(teamID); ok {
		state.FeatureEnabled = featureEnabled
		if strings.TrimSpace(state.BaseConfigPath) == "" {
			state.BaseConfigPath = basePath
		}
		if strings.TrimSpace(state.EffectiveConfigPath) == "" {
			state.EffectiveConfigPath = basePath
		}
		return state, nil
	}
	desired, err := summarizeCurrentDesired(basePath)
	if err != nil {
		return types.TeamReconfigureState{}, err
	}
	return types.TeamReconfigureState{
		TeamID:              teamID,
		BaseConfigPath:      basePath,
		EffectiveConfigPath: basePath,
		FeatureEnabled:      featureEnabled,
		Revision:            0,
		Desired:             desired,
	}, nil
}

func summarizeCurrentDesired(configPath string) (types.TeamConfiguredState, error) {
	cfg, err := config.Load(configPath)
	if err != nil {
		return types.TeamConfiguredState{}, err
	}
	tree, err := config.LoadTree(configPath)
	if err != nil {
		return types.TeamConfiguredState{}, err
	}
	rawCfg, err := loadRawConfig(configPath)
	if err != nil {
		return types.TeamConfiguredState{}, err
	}
	return buildDesiredSummary(rawCfg, cfg, tree), nil
}

func (c *teamController) applyChanges(basePath string, currentOverlay types.TeamOverlay, changes []types.TeamReconfigureChange) (types.TeamOverlay, []string, error) {
	overlay := cloneTeamOverlay(currentOverlay)
	currentRaw, err := materializeRawConfig(basePath, overlay)
	if err != nil {
		return types.TeamOverlay{}, nil, err
	}
	var warnings []string
	for _, change := range changes {
		warnings, err = applySingleChange(basePath, currentRaw, &overlay, warnings, change)
		if err != nil {
			return types.TeamOverlay{}, nil, err
		}
		currentRaw, err = materializeRawConfig(basePath, overlay)
		if err != nil {
			return types.TeamOverlay{}, nil, err
		}
	}
	return normalizeOverlay(overlay), warnings, nil
}

func applySingleChange(basePath string, currentRaw *config.Config, overlay *types.TeamOverlay, warnings []string, change types.TeamReconfigureChange) ([]string, error) {
	switch change.Kind {
	case types.TeamEntryWorkspace:
		return applyWorkspaceChange(basePath, currentRaw, overlay, warnings, change)
	case types.TeamEntryChild:
		return applyChildChange(basePath, currentRaw, overlay, warnings, change)
	case types.TeamEntryRootOrchestrator:
		return applyRootOrchestratorChange(overlay, warnings, change)
	default:
		return warnings, fmt.Errorf("unsupported change kind %q", change.Kind)
	}
}

func applyWorkspaceChange(basePath string, currentRaw *config.Config, overlay *types.TeamOverlay, warnings []string, change types.TeamReconfigureChange) ([]string, error) {
	name := strings.TrimSpace(change.Name)
	if name == "" {
		return warnings, fmt.Errorf("workspace change requires name")
	}
	_, exists := currentRaw.Workspaces[name]
	switch change.Op {
	case types.TeamChangeAdd:
		if change.Workspace == nil {
			return warnings, fmt.Errorf("workspace add requires workspace spec")
		}
		if exists {
			return warnings, fmt.Errorf("workspace %q already exists", name)
		}
		ensureWorkspaceMap(&overlay.AddedWorkspaces)
		spec := *change.Workspace
		spec.Dir = resolveOverlayDir(config.ConfigRootDir(basePath), spec.Dir)
		overlay.AddedWorkspaces[name] = spec
		delete(overlay.RemovedWorkspaces, name)
		delete(overlay.DisabledWorkspaces, name)
	case types.TeamChangeRemove:
		if !exists {
			warnings = append(warnings, fmt.Sprintf("workspace %q is already absent", name))
			return warnings, nil
		}
		delete(overlay.AddedWorkspaces, name)
		ensureBoolMap(&overlay.RemovedWorkspaces)
		overlay.RemovedWorkspaces[name] = true
		delete(overlay.DisabledWorkspaces, name)
	case types.TeamChangeDisable:
		if !exists {
			warnings = append(warnings, fmt.Sprintf("workspace %q is already absent/disabled", name))
			return warnings, nil
		}
		ensureBoolMap(&overlay.DisabledWorkspaces)
		overlay.DisabledWorkspaces[name] = true
		delete(overlay.RemovedWorkspaces, name)
	case types.TeamChangeEnable:
		if _, removed := overlay.RemovedWorkspaces[name]; removed {
			delete(overlay.RemovedWorkspaces, name)
			return warnings, nil
		}
		if _, disabled := overlay.DisabledWorkspaces[name]; disabled {
			delete(overlay.DisabledWorkspaces, name)
			return warnings, nil
		}
		warnings = append(warnings, fmt.Sprintf("workspace %q is already enabled", name))
	default:
		return warnings, fmt.Errorf("unsupported workspace op %q", change.Op)
	}
	return warnings, nil
}

func applyChildChange(basePath string, currentRaw *config.Config, overlay *types.TeamOverlay, warnings []string, change types.TeamReconfigureChange) ([]string, error) {
	name := strings.TrimSpace(change.Name)
	if name == "" {
		return warnings, fmt.Errorf("child change requires name")
	}
	_, exists := currentRaw.Children[name]
	switch change.Op {
	case types.TeamChangeAdd:
		if change.Child == nil {
			return warnings, fmt.Errorf("child add requires child spec")
		}
		if exists {
			return warnings, fmt.Errorf("child %q already exists", name)
		}
		ensureChildMap(&overlay.AddedChildren)
		spec := *change.Child
		spec.Dir = resolveOverlayDir(config.ConfigRootDir(basePath), spec.Dir)
		if strings.TrimSpace(spec.Dir) == "" {
			return warnings, fmt.Errorf("child add requires dir")
		}
		overlay.AddedChildren[name] = spec
		delete(overlay.RemovedChildren, name)
		delete(overlay.DisabledChildren, name)
	case types.TeamChangeRemove:
		if !exists {
			warnings = append(warnings, fmt.Sprintf("child %q is already absent", name))
			return warnings, nil
		}
		delete(overlay.AddedChildren, name)
		ensureBoolMap(&overlay.RemovedChildren)
		overlay.RemovedChildren[name] = true
		delete(overlay.DisabledChildren, name)
	case types.TeamChangeDisable:
		if !exists {
			warnings = append(warnings, fmt.Sprintf("child %q is already absent/disabled", name))
			return warnings, nil
		}
		ensureBoolMap(&overlay.DisabledChildren)
		overlay.DisabledChildren[name] = true
		delete(overlay.RemovedChildren, name)
	case types.TeamChangeEnable:
		if _, removed := overlay.RemovedChildren[name]; removed {
			delete(overlay.RemovedChildren, name)
			return warnings, nil
		}
		if _, disabled := overlay.DisabledChildren[name]; disabled {
			delete(overlay.DisabledChildren, name)
			return warnings, nil
		}
		warnings = append(warnings, fmt.Sprintf("child %q is already enabled", name))
	default:
		return warnings, fmt.Errorf("unsupported child op %q", change.Op)
	}
	return warnings, nil
}

func applyRootOrchestratorChange(overlay *types.TeamOverlay, warnings []string, change types.TeamReconfigureChange) ([]string, error) {
	switch change.Op {
	case types.TeamChangeDisable:
		flag := true
		overlay.DisableRootOrchestrator = &flag
	case types.TeamChangeEnable:
		flag := false
		overlay.DisableRootOrchestrator = &flag
	default:
		return warnings, fmt.Errorf("root orchestrator supports enable/disable only")
	}
	return warnings, nil
}

func materializeRawConfig(basePath string, overlay types.TeamOverlay) (*config.Config, error) {
	base, err := loadRawConfig(basePath)
	if err != nil {
		return nil, err
	}
	rootDir := config.ConfigRootDir(basePath)
	if base.Workspaces == nil {
		base.Workspaces = make(map[string]config.Workspace)
	}
	if base.Children == nil {
		base.Children = make(map[string]config.Child)
	}
	for name, ws := range base.Workspaces {
		ws.Dir = resolveOverlayDir(rootDir, ws.Dir)
		base.Workspaces[name] = ws
	}
	for name, child := range base.Children {
		child.Dir = resolveOverlayDir(rootDir, child.Dir)
		base.Children[name] = child
	}
	if overlay.DisableRootOrchestrator != nil {
		base.DisableRootOrchestrator = *overlay.DisableRootOrchestrator
	}
	for name := range overlay.RemovedWorkspaces {
		delete(base.Workspaces, name)
	}
	for name := range overlay.DisabledWorkspaces {
		delete(base.Workspaces, name)
	}
	for name, spec := range overlay.AddedWorkspaces {
		base.Workspaces[name] = config.Workspace{
			Dir:                       resolveOverlayDir(rootDir, spec.Dir),
			Description:               spec.Description,
			Shell:                     spec.Shell,
			Runtime:                   spec.Runtime,
			CodexModelReasoningEffort: spec.CodexModelReasoningEffort,
			Agent:                     spec.Agent,
			Instructions:              spec.Instructions,
			Env:                       cloneStringMap(spec.Env),
		}
	}
	for name := range overlay.RemovedChildren {
		delete(base.Children, name)
	}
	for name := range overlay.DisabledChildren {
		delete(base.Children, name)
	}
	for name, spec := range overlay.AddedChildren {
		base.Children[name] = config.Child{
			Dir:    resolveOverlayDir(rootDir, spec.Dir),
			Prefix: spec.Prefix,
		}
	}
	return base, nil
}

func (c *teamController) materializeState(basePath string, state types.TeamReconfigureState, persist bool) (string, types.TeamConfiguredState, *config.Config, *config.ProjectNode, error) {
	if !overlayHasChanges(state.Overlay) {
		cfg, err := config.Load(basePath)
		if err != nil {
			return "", types.TeamConfiguredState{}, nil, nil, err
		}
		tree, err := config.LoadTree(basePath)
		if err != nil {
			return "", types.TeamConfiguredState{}, nil, nil, err
		}
		rawCfg, err := loadRawConfig(basePath)
		if err != nil {
			return "", types.TeamConfiguredState{}, nil, nil, err
		}
		return basePath, buildDesiredSummary(rawCfg, cfg, tree), cfg, tree, nil
	}

	rawCfg, err := materializeRawConfig(basePath, state.Overlay)
	if err != nil {
		return "", types.TeamConfiguredState{}, nil, nil, err
	}
	path, err := c.effectiveConfigPath(basePath, persist)
	if err != nil {
		return "", types.TeamConfiguredState{}, nil, nil, err
	}
	if err := rawCfg.Save(path); err != nil {
		return "", types.TeamConfiguredState{}, nil, nil, err
	}
	cfg, err := config.Load(path)
	if err != nil {
		return "", types.TeamConfiguredState{}, nil, nil, err
	}
	tree, err := config.LoadTree(path)
	if err != nil {
		return "", types.TeamConfiguredState{}, nil, nil, err
	}
	return path, buildDesiredSummary(rawCfg, cfg, tree), cfg, tree, nil
}

func (c *teamController) effectiveConfigPath(basePath string, persist bool) (string, error) {
	dir := filepath.Join(c.stateDir, "managed-teams")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", err
	}
	hash := shortTeamHash(basePath)
	if persist {
		return filepath.Join(dir, hash+".yaml"), nil
	}
	return filepath.Join(dir, fmt.Sprintf("%s-plan-%d.yaml", hash, time.Now().UnixNano())), nil
}

func diffTeamActions(currentCfg *config.Config, currentTree *config.ProjectNode, nextCfg *config.Config, nextTree *config.ProjectNode) []types.TeamReconfigureAction {
	var actions []types.TeamReconfigureAction
	for name, ws := range currentCfg.Workspaces {
		if _, ok := nextCfg.Workspaces[name]; !ok {
			actions = append(actions, types.TeamReconfigureAction{
				Action: "destroy",
				Kind:   types.TeamEntryWorkspace,
				Name:   name,
				Dir:    ws.Dir,
			})
		}
	}
	for name, ws := range nextCfg.Workspaces {
		if _, ok := currentCfg.Workspaces[name]; !ok {
			actions = append(actions, types.TeamReconfigureAction{
				Action: "ensure",
				Kind:   types.TeamEntryWorkspace,
				Name:   name,
				Dir:    ws.Dir,
			})
		}
	}

	currentOrchs := collectOrchestratorInfo(currentTree)
	nextOrchs := collectOrchestratorInfo(nextTree)
	for name, info := range currentOrchs {
		if _, ok := nextOrchs[name]; !ok {
			actions = append(actions, types.TeamReconfigureAction{
				Action: "destroy",
				Kind:   types.TeamEntryRootOrchestrator,
				Name:   name,
				Dir:    info.dir,
			})
		}
	}
	for name, info := range nextOrchs {
		if _, ok := currentOrchs[name]; !ok {
			actions = append(actions, types.TeamReconfigureAction{
				Action: "ensure",
				Kind:   types.TeamEntryRootOrchestrator,
				Name:   name,
				Dir:    info.dir,
			})
		}
	}

	sort.Slice(actions, func(i, j int) bool {
		if actions[i].Kind != actions[j].Kind {
			return actions[i].Kind < actions[j].Kind
		}
		if actions[i].Name != actions[j].Name {
			return actions[i].Name < actions[j].Name
		}
		return actions[i].Action < actions[j].Action
	})
	return actions
}

type orchestratorInfo struct {
	dir string
}

func collectOrchestratorInfo(tree *config.ProjectNode) map[string]orchestratorInfo {
	result := make(map[string]orchestratorInfo)
	var walk func(node *config.ProjectNode)
	walk = func(node *config.ProjectNode) {
		if node == nil {
			return
		}
		if !(node.Prefix == "" && node.DisableRootOrchestrator) {
			if dir, err := orchestratorDirForNode(node); err == nil {
				name := "orchestrator"
				if node.Prefix != "" {
					name = node.Prefix + ".orchestrator"
				}
				result[name] = orchestratorInfo{dir: dir}
			}
		}
		for _, child := range node.Children {
			walk(child)
		}
	}
	walk(tree)
	return result
}

func buildDesiredSummary(rawCfg *config.Config, loadedCfg *config.Config, tree *config.ProjectNode) types.TeamConfiguredState {
	summary := types.TeamConfiguredState{
		RootOrchestratorEnabled: tree != nil && !tree.DisableRootOrchestrator,
	}
	for name := range loadedCfg.Workspaces {
		summary.Workspaces = append(summary.Workspaces, name)
	}
	sort.Strings(summary.Workspaces)
	for name := range rawCfg.Children {
		summary.Children = append(summary.Children, name)
	}
	sort.Strings(summary.Children)
	orchs := collectOrchestratorInfo(tree)
	for name := range orchs {
		summary.Orchestrators = append(summary.Orchestrators, name)
	}
	sort.Strings(summary.Orchestrators)
	return summary
}

func canonicalConfigPath(configPath string) (string, error) {
	if strings.TrimSpace(configPath) == "" {
		return "", fmt.Errorf("config_path is required")
	}
	path, err := filepath.Abs(strings.TrimSpace(configPath))
	if err != nil {
		return "", fmt.Errorf("resolve config path: %w", err)
	}
	return path, nil
}

func loadRawConfig(path string) (*config.Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var cfg config.Config
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, err
	}
	if cfg.Workspaces == nil {
		cfg.Workspaces = make(map[string]config.Workspace)
	}
	if cfg.Children == nil {
		cfg.Children = make(map[string]config.Child)
	}
	return &cfg, nil
}

func resolveOverlayDir(baseDir, value string) string {
	if strings.TrimSpace(value) == "" {
		value = "."
	}
	if strings.HasPrefix(value, "~/") {
		home, _ := os.UserHomeDir()
		return filepath.Join(home, value[2:])
	}
	if filepath.IsAbs(value) {
		return value
	}
	return filepath.Join(baseDir, value)
}

func overlayHasChanges(overlay types.TeamOverlay) bool {
	return overlay.DisableRootOrchestrator != nil ||
		len(overlay.AddedWorkspaces) > 0 ||
		len(overlay.RemovedWorkspaces) > 0 ||
		len(overlay.DisabledWorkspaces) > 0 ||
		len(overlay.AddedChildren) > 0 ||
		len(overlay.RemovedChildren) > 0 ||
		len(overlay.DisabledChildren) > 0
}

func normalizeOverlay(overlay types.TeamOverlay) types.TeamOverlay {
	if len(overlay.AddedWorkspaces) == 0 {
		overlay.AddedWorkspaces = nil
	}
	if len(overlay.RemovedWorkspaces) == 0 {
		overlay.RemovedWorkspaces = nil
	}
	if len(overlay.DisabledWorkspaces) == 0 {
		overlay.DisabledWorkspaces = nil
	}
	if len(overlay.AddedChildren) == 0 {
		overlay.AddedChildren = nil
	}
	if len(overlay.RemovedChildren) == 0 {
		overlay.RemovedChildren = nil
	}
	if len(overlay.DisabledChildren) == 0 {
		overlay.DisabledChildren = nil
	}
	return overlay
}

func ensureWorkspaceMap(dst *map[string]types.TeamWorkspaceSpec) {
	if *dst == nil {
		*dst = make(map[string]types.TeamWorkspaceSpec)
	}
}

func ensureChildMap(dst *map[string]types.TeamChildSpec) {
	if *dst == nil {
		*dst = make(map[string]types.TeamChildSpec)
	}
}

func ensureBoolMap(dst *map[string]bool) {
	if *dst == nil {
		*dst = make(map[string]bool)
	}
}

func cloneStringMap(src map[string]string) map[string]string {
	if len(src) == 0 {
		return nil
	}
	dst := make(map[string]string, len(src))
	for key, value := range src {
		dst[key] = value
	}
	return dst
}

func shortTeamHash(value string) string {
	sum := sha1.Sum([]byte(value))
	return hex.EncodeToString(sum[:6])
}

func newTicketToken(teamID string, now time.Time) string {
	return shortTeamHash(teamID) + "-" + fmt.Sprintf("%d", now.UnixNano())
}

func orchestratorDirForNode(node *config.ProjectNode) (string, error) {
	if node == nil {
		return "", fmt.Errorf("nil project node")
	}
	if node.Prefix == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		return filepath.Join(home, ".ax", "orchestrator"), nil
	}
	safe := strings.ReplaceAll(node.Prefix, ".", "_")
	return filepath.Join(node.Dir, ".ax", "orchestrator-"+safe), nil
}
