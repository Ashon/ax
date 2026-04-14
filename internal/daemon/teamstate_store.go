package daemon

import (
	"encoding/json"
	"os"
	"path/filepath"
	"sync"

	"github.com/ashon/ax/internal/types"
)

type TeamStateStore struct {
	mu       sync.RWMutex
	states   map[string]*types.TeamReconfigureState
	filePath string
}

func NewTeamStateStore(stateDir string) *TeamStateStore {
	return &TeamStateStore{
		states:   make(map[string]*types.TeamReconfigureState),
		filePath: filepath.Join(stateDir, "team_states.json"),
	}
}

func (s *TeamStateStore) Load() error {
	s.mu.Lock()
	defer s.mu.Unlock()

	data, err := os.ReadFile(s.filePath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	if len(data) == 0 {
		s.states = make(map[string]*types.TeamReconfigureState)
		return nil
	}
	var states []types.TeamReconfigureState
	if err := json.Unmarshal(data, &states); err != nil {
		return err
	}
	loaded := make(map[string]*types.TeamReconfigureState, len(states))
	for _, state := range states {
		cp := cloneTeamState(state)
		loaded[state.TeamID] = &cp
	}
	s.states = loaded
	return nil
}

func (s *TeamStateStore) Get(teamID string) (types.TeamReconfigureState, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	state, ok := s.states[teamID]
	if !ok {
		return types.TeamReconfigureState{}, false
	}
	return cloneTeamState(*state), true
}

func (s *TeamStateStore) Put(state types.TeamReconfigureState) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	cp := cloneTeamState(state)
	s.states[state.TeamID] = &cp
	return s.persistLocked()
}

func (s *TeamStateStore) persistLocked() error {
	states := make([]types.TeamReconfigureState, 0, len(s.states))
	for _, state := range s.states {
		states = append(states, cloneTeamState(*state))
	}
	data, err := json.Marshal(states)
	if err != nil {
		return err
	}
	return writeFileAtomic(s.filePath, data, 0o600)
}

func cloneTeamState(state types.TeamReconfigureState) types.TeamReconfigureState {
	cp := state
	cp.Overlay = cloneTeamOverlay(state.Overlay)
	cp.Desired.Workspaces = append([]string(nil), state.Desired.Workspaces...)
	cp.Desired.Children = append([]string(nil), state.Desired.Children...)
	cp.Desired.Orchestrators = append([]string(nil), state.Desired.Orchestrators...)
	if state.LastApply != nil {
		last := *state.LastApply
		last.Actions = append([]types.TeamReconfigureAction(nil), state.LastApply.Actions...)
		cp.LastApply = &last
	}
	return cp
}

func cloneTeamOverlay(overlay types.TeamOverlay) types.TeamOverlay {
	cp := overlay
	if overlay.DisableRootOrchestrator != nil {
		flag := *overlay.DisableRootOrchestrator
		cp.DisableRootOrchestrator = &flag
	}
	cp.AddedWorkspaces = cloneTeamWorkspaceMap(overlay.AddedWorkspaces)
	cp.RemovedWorkspaces = cloneBoolMap(overlay.RemovedWorkspaces)
	cp.DisabledWorkspaces = cloneBoolMap(overlay.DisabledWorkspaces)
	cp.AddedChildren = cloneTeamChildMap(overlay.AddedChildren)
	cp.RemovedChildren = cloneBoolMap(overlay.RemovedChildren)
	cp.DisabledChildren = cloneBoolMap(overlay.DisabledChildren)
	return cp
}

func cloneTeamWorkspaceMap(src map[string]types.TeamWorkspaceSpec) map[string]types.TeamWorkspaceSpec {
	if len(src) == 0 {
		return nil
	}
	dst := make(map[string]types.TeamWorkspaceSpec, len(src))
	for key, spec := range src {
		cp := spec
		if len(spec.Env) > 0 {
			cp.Env = make(map[string]string, len(spec.Env))
			for envKey, envVal := range spec.Env {
				cp.Env[envKey] = envVal
			}
		}
		dst[key] = cp
	}
	return dst
}

func cloneTeamChildMap(src map[string]types.TeamChildSpec) map[string]types.TeamChildSpec {
	if len(src) == 0 {
		return nil
	}
	dst := make(map[string]types.TeamChildSpec, len(src))
	for key, spec := range src {
		dst[key] = spec
	}
	return dst
}

func cloneBoolMap(src map[string]bool) map[string]bool {
	if len(src) == 0 {
		return nil
	}
	dst := make(map[string]bool, len(src))
	for key, value := range src {
		dst[key] = value
	}
	return dst
}
