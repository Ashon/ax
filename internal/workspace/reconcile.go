package workspace

import (
	"crypto/sha1"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/tmux"
)

const (
	reconcileStateVersion = 1
	reconcileStateFile    = ".runtime-state.json"
)

var (
	listTmuxSessions = tmux.ListSessions
	tmuxSessionIdle  = tmux.IsIdle
)

type ReconcileOptions struct {
	DaemonRunning          bool
	AllowDisruptiveChanges bool
}

type ReconcileAction struct {
	Kind      string `json:"kind"`
	Name      string `json:"name"`
	Operation string `json:"operation"`
	Details   string `json:"details,omitempty"`
}

type ReconcileReport struct {
	Actions                   []ReconcileAction `json:"actions,omitempty"`
	RootManualRestartRequired bool              `json:"root_manual_restart_required,omitempty"`
	RootManualRestartReasons  []string          `json:"root_manual_restart_reasons,omitempty"`
}

func (r *ReconcileReport) addAction(kind, name, operation, details string) {
	r.Actions = append(r.Actions, ReconcileAction{
		Kind:      kind,
		Name:      name,
		Operation: operation,
		Details:   details,
	})
}

func (r *ReconcileReport) requireRootManualRestart(reason string) {
	r.RootManualRestartRequired = true
	if strings.TrimSpace(reason) == "" {
		return
	}
	for _, existing := range r.RootManualRestartReasons {
		if existing == reason {
			return
		}
	}
	r.RootManualRestartReasons = append(r.RootManualRestartReasons, reason)
}

type DesiredState struct {
	SocketPath    string
	ConfigPath    string
	Workspaces    map[string]DesiredWorkspace
	Orchestrators map[string]DesiredOrchestrator
}

type DesiredWorkspace struct {
	Name      string
	Workspace config.Workspace
}

type DesiredOrchestrator struct {
	Name           string
	Node           *config.ProjectNode
	ParentName     string
	ArtifactDir    string
	Runtime        string
	Root           bool
	ManagedSession bool
	PromptHash     string
}

type Reconciler struct {
	socketPath string
	configPath string
	manager    *Manager
}

type reconcileState struct {
	Version       int                          `json:"version"`
	SocketPath    string                       `json:"socket_path,omitempty"`
	ConfigPath    string                       `json:"config_path,omitempty"`
	Workspaces    map[string]workspaceState    `json:"workspaces,omitempty"`
	Orchestrators map[string]orchestratorState `json:"orchestrators,omitempty"`
}

type workspaceState struct {
	Name             string            `json:"name"`
	Dir              string            `json:"dir"`
	Runtime          string            `json:"runtime"`
	Agent            string            `json:"agent,omitempty"`
	Shell            string            `json:"shell,omitempty"`
	Env              map[string]string `json:"env,omitempty"`
	InstructionsHash string            `json:"instructions_hash,omitempty"`
}

type orchestratorState struct {
	Name           string `json:"name"`
	ArtifactDir    string `json:"artifact_dir"`
	Runtime        string `json:"runtime"`
	ParentName     string `json:"parent_name,omitempty"`
	PromptHash     string `json:"prompt_hash"`
	ManagedSession bool   `json:"managed_session,omitempty"`
	Root           bool   `json:"root,omitempty"`
}

type sessionSnapshot struct {
	Exists   bool
	Attached bool
	Idle     bool
}

func NewReconciler(socketPath, configPath string) *Reconciler {
	return &Reconciler{
		socketPath: daemonutil.ExpandSocketPath(socketPath),
		configPath: cleanPath(configPath),
		manager:    NewManager(socketPath, configPath),
	}
}

func BuildDesiredState(cfg *config.Config, tree *config.ProjectNode, socketPath, configPath string, includeRoot bool) (*DesiredState, error) {
	desired := &DesiredState{
		SocketPath:    daemonutil.ExpandSocketPath(socketPath),
		ConfigPath:    cleanPath(configPath),
		Workspaces:    make(map[string]DesiredWorkspace),
		Orchestrators: make(map[string]DesiredOrchestrator),
	}

	if cfg != nil {
		for name, ws := range cfg.Workspaces {
			desired.Workspaces[name] = DesiredWorkspace{
				Name:      name,
				Workspace: ws,
			}
		}
	}

	if tree != nil {
		if err := appendDesiredOrchestrators(desired, tree, "", includeRoot); err != nil {
			return nil, err
		}
	}

	return desired, nil
}

func appendDesiredOrchestrators(desired *DesiredState, node *config.ProjectNode, parentName string, includeSelf bool) error {
	if node == nil {
		return nil
	}

	selfName := OrchestratorName(node.Prefix)
	runtime := agent.NormalizeRuntime(node.OrchestratorRuntime)
	if _, err := agent.Get(runtime); err != nil {
		return err
	}

	childParentName := parentName
	if includeSelf {
		orchDir, err := OrchestratorDirForNode(node)
		if err != nil {
			return err
		}
		prompt := buildOrchestratorPromptContent(node, node.Prefix, parentName)
		desired.Orchestrators[selfName] = DesiredOrchestrator{
			Name:           selfName,
			Node:           node,
			ParentName:     parentName,
			ArtifactDir:    orchDir,
			Runtime:        runtime,
			Root:           node.Prefix == "",
			ManagedSession: node.Prefix != "",
			PromptHash:     hashText(prompt),
		}
		childParentName = selfName
	}

	for _, child := range node.Children {
		if err := appendDesiredOrchestrators(desired, child, childParentName, true); err != nil {
			return err
		}
	}
	return nil
}

func (r *Reconciler) ReconcileDesiredState(desired *DesiredState, opts ReconcileOptions) (ReconcileReport, error) {
	var report ReconcileReport
	if desired == nil {
		return report, fmt.Errorf("desired state is nil")
	}

	statePath := reconcileStatePath(r.configPath)
	previous, err := loadReconcileState(statePath)
	if err != nil {
		return report, err
	}

	sessionNames := desiredSessionNames(previous, desired)
	sessions, err := loadSessionSnapshots(sessionNames)
	if err != nil {
		return report, err
	}

	next := newReconcileState()
	next.SocketPath = desired.SocketPath
	next.ConfigPath = desired.ConfigPath
	globalChanged := previous.SocketPath != desired.SocketPath || previous.ConfigPath != desired.ConfigPath

	workspaceNames := sortedDesiredWorkspaceNames(desired.Workspaces)
	for _, name := range workspaceNames {
		entry := desired.Workspaces[name]
		record := desiredWorkspaceState(entry)
		prevRecord, hadPrev := previous.Workspaces[name]
		session := sessions[name]
		matches := hadPrev && workspaceStateMatches(prevRecord, record) && !globalChanged

		switch {
		case matches:
			if err := EnsureArtifacts(name, entry.Workspace, r.socketPath, r.configPath); err != nil {
				return report, err
			}
			if opts.DaemonRunning && !session.Exists {
				if err := r.manager.Create(name, entry.Workspace); err != nil {
					return report, err
				}
				report.addAction("workspace", name, "create", "session was missing and has been started")
			}
			next.Workspaces[name] = record
		default:
			action := "create"
			if hadPrev {
				action = "restart"
			}
			if session.Exists && !opts.AllowDisruptiveChanges {
				report.addAction("workspace", name, "blocked_"+action, "reconcile mode forbids disrupting an existing session")
				if hadPrev {
					next.Workspaces[name] = prevRecord
				}
				continue
			}
			if session.Exists {
				if ok, reason := canDisruptSession(session); !ok {
					report.addAction("workspace", name, "blocked_"+action, reason)
					if hadPrev {
						next.Workspaces[name] = prevRecord
					}
					continue
				}
			}
			cleanupDir := record.Dir
			if hadPrev {
				cleanupDir = prevRecord.Dir
			}
			if err := CleanupWorkspaceState(name, cleanupDir); err != nil {
				return report, err
			}
			if err := EnsureArtifacts(name, entry.Workspace, r.socketPath, r.configPath); err != nil {
				return report, err
			}
			if opts.DaemonRunning {
				if err := r.manager.Create(name, entry.Workspace); err != nil {
					return report, err
				}
			}
			details := "generated artifacts refreshed"
			if opts.DaemonRunning {
				details = "generated artifacts refreshed and session started"
			}
			report.addAction("workspace", name, action, details)
			next.Workspaces[name] = record
		}
	}

	for _, name := range sortedWorkspaceStateNames(previous.Workspaces) {
		if _, keep := desired.Workspaces[name]; keep {
			continue
		}
		prevRecord := previous.Workspaces[name]
		session := sessions[name]
		if session.Exists && !opts.AllowDisruptiveChanges {
			report.addAction("workspace", name, "blocked_remove", "reconcile mode forbids disrupting an existing session")
			next.Workspaces[name] = prevRecord
			continue
		}
		if session.Exists {
			if ok, reason := canDisruptSession(session); !ok {
				report.addAction("workspace", name, "blocked_remove", reason)
				next.Workspaces[name] = prevRecord
				continue
			}
		}
		if err := CleanupWorkspaceState(name, prevRecord.Dir); err != nil {
			return report, err
		}
		report.addAction("workspace", name, "remove", "generated artifacts cleaned up")
	}

	orchestratorNames := sortedDesiredOrchestratorNames(desired.Orchestrators)
	for _, name := range orchestratorNames {
		entry := desired.Orchestrators[name]
		record := desiredOrchestratorState(entry)
		prevRecord, hadPrev := previous.Orchestrators[name]
		session := sessions[name]
		matches := hadPrev && orchestratorStateMatches(prevRecord, record) && !globalChanged

		if entry.Root {
			if err := EnsureOrchestrator(entry.Node, entry.ParentName, r.socketPath, r.configPath, false); err != nil {
				return report, err
			}
			if !matches {
				report.requireRootManualRestart("root orchestrator artifacts changed; manual relaunch is required")
				if hadPrev {
					report.addAction("orchestrator", name, "manual_restart_required", "root foreground orchestrator is not hot-reloaded")
				} else {
					report.addAction("orchestrator", name, "create_artifacts", "root orchestrator artifacts created")
				}
			}
			next.Orchestrators[name] = record
			continue
		}

		if matches {
			if err := EnsureOrchestrator(entry.Node, entry.ParentName, r.socketPath, r.configPath, opts.DaemonRunning); err != nil {
				return report, err
			}
			if opts.DaemonRunning && !session.Exists && entry.ManagedSession {
				report.addAction("orchestrator", name, "create", "session was missing and has been started")
			}
			next.Orchestrators[name] = record
			continue
		}

		action := "create"
		if hadPrev {
			action = "restart"
		}
		if session.Exists && !opts.AllowDisruptiveChanges {
			report.addAction("orchestrator", name, "blocked_"+action, "reconcile mode forbids disrupting an existing session")
			if hadPrev {
				next.Orchestrators[name] = prevRecord
			}
			continue
		}
		if session.Exists {
			if ok, reason := canDisruptSession(session); !ok {
				report.addAction("orchestrator", name, "blocked_"+action, reason)
				if hadPrev {
					next.Orchestrators[name] = prevRecord
				}
				continue
			}
		}
		cleanupDir := record.ArtifactDir
		if hadPrev {
			cleanupDir = prevRecord.ArtifactDir
		}
		if err := CleanupOrchestratorState(name, cleanupDir); err != nil {
			return report, err
		}
		if err := EnsureOrchestrator(entry.Node, entry.ParentName, r.socketPath, r.configPath, opts.DaemonRunning); err != nil {
			return report, err
		}
		details := "generated artifacts refreshed"
		if opts.DaemonRunning && entry.ManagedSession {
			details = "generated artifacts refreshed and session started"
		}
		report.addAction("orchestrator", name, action, details)
		next.Orchestrators[name] = record
	}

	for _, name := range sortedOrchestratorStateNames(previous.Orchestrators) {
		if _, keep := desired.Orchestrators[name]; keep {
			continue
		}
		prevRecord := previous.Orchestrators[name]
		session := sessions[name]
		if session.Exists && !opts.AllowDisruptiveChanges {
			report.addAction("orchestrator", name, "blocked_remove", "reconcile mode forbids disrupting an existing session")
			next.Orchestrators[name] = prevRecord
			continue
		}
		if session.Exists {
			if ok, reason := canDisruptSession(session); !ok {
				report.addAction("orchestrator", name, "blocked_remove", reason)
				next.Orchestrators[name] = prevRecord
				continue
			}
		}
		if err := CleanupOrchestratorState(name, prevRecord.ArtifactDir); err != nil {
			return report, err
		}
		report.addAction("orchestrator", name, "remove", "generated artifacts cleaned up")
	}

	if err := saveReconcileState(statePath, next); err != nil {
		return report, err
	}

	return report, nil
}

func desiredSessionNames(previous *reconcileState, desired *DesiredState) []string {
	names := make(map[string]struct{})
	for name := range previous.Workspaces {
		names[name] = struct{}{}
	}
	for name := range previous.Orchestrators {
		names[name] = struct{}{}
	}
	for name := range desired.Workspaces {
		names[name] = struct{}{}
	}
	for name := range desired.Orchestrators {
		names[name] = struct{}{}
	}

	out := make([]string, 0, len(names))
	for name := range names {
		out = append(out, name)
	}
	sort.Strings(out)
	return out
}

func loadSessionSnapshots(names []string) (map[string]sessionSnapshot, error) {
	result := make(map[string]sessionSnapshot, len(names))
	if len(names) == 0 {
		return result, nil
	}

	sessions, err := listTmuxSessions()
	if err != nil {
		return nil, err
	}
	byName := make(map[string]tmux.SessionInfo, len(sessions))
	for _, session := range sessions {
		byName[session.Workspace] = session
	}

	for _, name := range names {
		info, ok := byName[name]
		if !ok {
			continue
		}
		snapshot := sessionSnapshot{
			Exists:   true,
			Attached: info.Attached,
			Idle:     !info.Attached,
		}
		if !info.Attached {
			snapshot.Idle = tmuxSessionIdle(name)
		}
		result[name] = snapshot
	}
	return result, nil
}

func canDisruptSession(snapshot sessionSnapshot) (bool, string) {
	if !snapshot.Exists {
		return true, ""
	}
	if snapshot.Attached {
		return false, "tmux session is attached"
	}
	if !snapshot.Idle {
		return false, "tmux session is not idle"
	}
	return true, ""
}

func desiredWorkspaceState(entry DesiredWorkspace) workspaceState {
	return workspaceState{
		Name:             entry.Name,
		Dir:              cleanPath(entry.Workspace.Dir),
		Runtime:          agent.NormalizeRuntime(entry.Workspace.Runtime),
		Agent:            strings.TrimSpace(entry.Workspace.Agent),
		Shell:            strings.TrimSpace(entry.Workspace.Shell),
		Env:              cloneStringMap(entry.Workspace.Env),
		InstructionsHash: hashText(strings.TrimSpace(entry.Workspace.Instructions)),
	}
}

func desiredOrchestratorState(entry DesiredOrchestrator) orchestratorState {
	return orchestratorState{
		Name:           entry.Name,
		ArtifactDir:    cleanPath(entry.ArtifactDir),
		Runtime:        entry.Runtime,
		ParentName:     entry.ParentName,
		PromptHash:     entry.PromptHash,
		ManagedSession: entry.ManagedSession,
		Root:           entry.Root,
	}
}

func workspaceStateMatches(a, b workspaceState) bool {
	return a.Name == b.Name &&
		a.Dir == b.Dir &&
		a.Runtime == b.Runtime &&
		a.Agent == b.Agent &&
		a.Shell == b.Shell &&
		a.InstructionsHash == b.InstructionsHash &&
		stringMapEqual(a.Env, b.Env)
}

func orchestratorStateMatches(a, b orchestratorState) bool {
	return a.Name == b.Name &&
		a.ArtifactDir == b.ArtifactDir &&
		a.Runtime == b.Runtime &&
		a.ParentName == b.ParentName &&
		a.PromptHash == b.PromptHash &&
		a.ManagedSession == b.ManagedSession &&
		a.Root == b.Root
}

func newReconcileState() *reconcileState {
	return &reconcileState{
		Version:       reconcileStateVersion,
		Workspaces:    make(map[string]workspaceState),
		Orchestrators: make(map[string]orchestratorState),
	}
}

func loadReconcileState(path string) (*reconcileState, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return newReconcileState(), nil
		}
		return nil, fmt.Errorf("read reconcile state %s: %w", path, err)
	}

	state := newReconcileState()
	if err := json.Unmarshal(data, state); err != nil {
		return nil, fmt.Errorf("parse reconcile state %s: %w", path, err)
	}
	if state.Workspaces == nil {
		state.Workspaces = make(map[string]workspaceState)
	}
	if state.Orchestrators == nil {
		state.Orchestrators = make(map[string]orchestratorState)
	}
	return state, nil
}

func saveReconcileState(path string, state *reconcileState) error {
	if state == nil {
		state = newReconcileState()
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create reconcile state dir: %w", err)
	}
	data, err := json.MarshalIndent(state, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal reconcile state: %w", err)
	}
	if err := os.WriteFile(path, append(data, '\n'), 0o644); err != nil {
		return fmt.Errorf("write reconcile state %s: %w", path, err)
	}
	return nil
}

func reconcileStatePath(configPath string) string {
	base := filepath.Dir(configPath)
	if strings.TrimSpace(base) == "" || base == "." {
		base = "."
	}
	return filepath.Join(base, reconcileStateFile)
}

func sortedDesiredWorkspaceNames(entries map[string]DesiredWorkspace) []string {
	names := make([]string, 0, len(entries))
	for name := range entries {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}

func sortedWorkspaceStateNames(entries map[string]workspaceState) []string {
	names := make([]string, 0, len(entries))
	for name := range entries {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}

func sortedDesiredOrchestratorNames(entries map[string]DesiredOrchestrator) []string {
	names := make([]string, 0, len(entries))
	for name := range entries {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}

func sortedOrchestratorStateNames(entries map[string]orchestratorState) []string {
	names := make([]string, 0, len(entries))
	for name := range entries {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
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

func stringMapEqual(a, b map[string]string) bool {
	if len(a) != len(b) {
		return false
	}
	for key, value := range a {
		if b[key] != value {
			return false
		}
	}
	return true
}

func hashText(value string) string {
	sum := sha1.Sum([]byte(value))
	return hex.EncodeToString(sum[:])
}

func cleanPath(path string) string {
	if strings.TrimSpace(path) == "" {
		return ""
	}
	return filepath.Clean(path)
}
