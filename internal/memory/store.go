package memory

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/types"
	"github.com/google/uuid"
)

const (
	FileName        = "memories.json"
	GlobalScope     = "global"
	RootProjectName = "root"
	DefaultKind     = "fact"
	DefaultLimit    = 10
	DefaultPromptN  = 6
)

type Query struct {
	Scopes            []string
	Kind              string
	Tags              []string
	IncludeSuperseded bool
	Limit             int
}

type Store struct {
	mu       sync.RWMutex
	filePath string
	entries  map[string]*types.Memory
}

func NewStore(stateDir string) *Store {
	return &Store{
		filePath: filepath.Join(stateDir, FileName),
		entries:  make(map[string]*types.Memory),
	}
}

func NewStoreForSocket(socketPath string) *Store {
	sp := daemonutil.ExpandSocketPath(socketPath)
	return NewStore(filepath.Dir(sp))
}

func ProjectScope(prefix string) string {
	prefix = strings.TrimSpace(prefix)
	if prefix == "" {
		prefix = RootProjectName
	}
	return "project:" + prefix
}

func WorkspaceScope(name string) string {
	return "workspace:" + strings.TrimSpace(name)
}

func TaskScope(id string) string {
	return "task:" + strings.TrimSpace(id)
}

func NormalizeKind(kind string) string {
	kind = strings.ToLower(strings.TrimSpace(kind))
	if kind == "" {
		return DefaultKind
	}
	return kind
}

func NormalizeScope(scope string) string {
	scope = strings.TrimSpace(scope)
	if scope == "" {
		return ""
	}
	if strings.EqualFold(scope, GlobalScope) {
		return GlobalScope
	}
	if strings.HasPrefix(strings.ToLower(scope), "project:") {
		value := strings.TrimSpace(scope[len("project:"):])
		if value == "" {
			value = RootProjectName
		}
		return "project:" + value
	}
	if strings.HasPrefix(strings.ToLower(scope), "workspace:") {
		value := strings.TrimSpace(scope[len("workspace:"):])
		if value == "" {
			return ""
		}
		return "workspace:" + value
	}
	if strings.HasPrefix(strings.ToLower(scope), "task:") {
		value := strings.TrimSpace(scope[len("task:"):])
		if value == "" {
			return ""
		}
		return "task:" + value
	}
	return scope
}

func LoadPromptMemories(socketPath string, scopes []string, limit int) ([]types.Memory, error) {
	store := NewStoreForSocket(socketPath)
	if err := store.Load(); err != nil {
		return nil, err
	}
	return store.List(Query{
		Scopes: scopes,
		Limit:  limit,
	})
}

func (s *Store) Load() error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.filePath == "" {
		return nil
	}
	data, err := os.ReadFile(s.filePath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	if len(data) == 0 {
		s.entries = make(map[string]*types.Memory)
		return nil
	}
	var entries []types.Memory
	if err := json.Unmarshal(data, &entries); err != nil {
		return err
	}
	loaded := make(map[string]*types.Memory, len(entries))
	for _, entry := range entries {
		cp := copyMemory(&entry)
		loaded[cp.ID] = cp
	}
	s.entries = loaded
	return nil
}

func (s *Store) Remember(scope, kind, subject, content string, tags []string, createdBy string, supersedes []string) (*types.Memory, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	scope = NormalizeScope(scope)
	kind = NormalizeKind(kind)
	subject = strings.TrimSpace(subject)
	content = strings.TrimSpace(content)
	createdBy = strings.TrimSpace(createdBy)
	tags = normalizeTags(tags)
	supersedes = normalizeIDs(supersedes)

	if scope == "" {
		return nil, fmt.Errorf("memory scope is required")
	}
	if content == "" {
		return nil, fmt.Errorf("memory content is required")
	}
	if createdBy == "" {
		return nil, fmt.Errorf("memory created_by is required")
	}

	now := time.Now()
	entry := &types.Memory{
		ID:         uuid.New().String(),
		Scope:      scope,
		Kind:       kind,
		Subject:    subject,
		Content:    content,
		Tags:       tags,
		CreatedBy:  createdBy,
		Supersedes: supersedes,
		CreatedAt:  now,
		UpdatedAt:  now,
	}
	type supersedeSnapshot struct {
		supersededAt *time.Time
		supersededBy string
		updatedAt    time.Time
	}
	previous := make(map[string]supersedeSnapshot, len(supersedes))
	for _, id := range supersedes {
		target, ok := s.entries[id]
		if !ok {
			return nil, fmt.Errorf("memory %q not found", id)
		}
		if target.SupersededAt != nil {
			return nil, fmt.Errorf("memory %q is already superseded", id)
		}
		previous[id] = supersedeSnapshot{
			supersededAt: target.SupersededAt,
			supersededBy: target.SupersededBy,
			updatedAt:    target.UpdatedAt,
		}
		target.SupersededAt = &now
		target.SupersededBy = entry.ID
		target.UpdatedAt = now
	}
	s.entries[entry.ID] = entry
	if err := s.persistLocked(); err != nil {
		delete(s.entries, entry.ID)
		for _, id := range supersedes {
			target := s.entries[id]
			snapshot := previous[id]
			target.SupersededAt = snapshot.supersededAt
			target.SupersededBy = snapshot.supersededBy
			target.UpdatedAt = snapshot.updatedAt
		}
		return nil, err
	}
	return copyMemory(entry), nil
}

func (s *Store) List(query Query) ([]types.Memory, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.listLocked(query), nil
}

func (s *Store) listLocked(query Query) []types.Memory {
	scopes := normalizeScopes(query.Scopes)
	kind := NormalizeKind(query.Kind)
	useKind := strings.TrimSpace(query.Kind) != ""
	tags := normalizeTags(query.Tags)
	limit := query.Limit
	if limit <= 0 {
		limit = DefaultLimit
	}

	result := make([]types.Memory, 0, len(s.entries))
	for _, entry := range s.entries {
		if !query.IncludeSuperseded && entry.SupersededAt != nil {
			continue
		}
		if len(scopes) > 0 && !slices.Contains(scopes, entry.Scope) {
			continue
		}
		if useKind && entry.Kind != kind {
			continue
		}
		if len(tags) > 0 && !hasAnyTag(entry.Tags, tags) {
			continue
		}
		result = append(result, *copyMemory(entry))
	}

	sort.Slice(result, func(i, j int) bool {
		leftActive := result[i].SupersededAt == nil
		rightActive := result[j].SupersededAt == nil
		if leftActive != rightActive {
			return leftActive
		}
		if !result[i].UpdatedAt.Equal(result[j].UpdatedAt) {
			return result[i].UpdatedAt.After(result[j].UpdatedAt)
		}
		if !result[i].CreatedAt.Equal(result[j].CreatedAt) {
			return result[i].CreatedAt.After(result[j].CreatedAt)
		}
		return result[i].ID < result[j].ID
	})

	if limit > 0 && len(result) > limit {
		result = result[:limit]
	}
	return result
}

func (s *Store) persistLocked() error {
	if s.filePath == "" {
		return nil
	}
	entries := make([]types.Memory, 0, len(s.entries))
	for _, entry := range s.entries {
		entries = append(entries, *copyMemory(entry))
	}
	sort.Slice(entries, func(i, j int) bool {
		if !entries[i].CreatedAt.Equal(entries[j].CreatedAt) {
			return entries[i].CreatedAt.Before(entries[j].CreatedAt)
		}
		return entries[i].ID < entries[j].ID
	})
	data, err := json.Marshal(entries)
	if err != nil {
		return err
	}
	return writeFileAtomic(s.filePath, data, 0o600)
}

func copyMemory(entry *types.Memory) *types.Memory {
	if entry == nil {
		return nil
	}
	cp := *entry
	cp.Tags = append([]string(nil), entry.Tags...)
	cp.Supersedes = append([]string(nil), entry.Supersedes...)
	return &cp
}

func normalizeScopes(scopes []string) []string {
	if len(scopes) == 0 {
		return nil
	}
	seen := make(map[string]struct{}, len(scopes))
	result := make([]string, 0, len(scopes))
	for _, scope := range scopes {
		scope = NormalizeScope(scope)
		if scope == "" {
			continue
		}
		if _, ok := seen[scope]; ok {
			continue
		}
		seen[scope] = struct{}{}
		result = append(result, scope)
	}
	sort.Strings(result)
	return result
}

func normalizeTags(tags []string) []string {
	if len(tags) == 0 {
		return nil
	}
	seen := make(map[string]struct{}, len(tags))
	result := make([]string, 0, len(tags))
	for _, tag := range tags {
		tag = strings.ToLower(strings.TrimSpace(tag))
		if tag == "" {
			continue
		}
		if _, ok := seen[tag]; ok {
			continue
		}
		seen[tag] = struct{}{}
		result = append(result, tag)
	}
	sort.Strings(result)
	return result
}

func normalizeIDs(ids []string) []string {
	if len(ids) == 0 {
		return nil
	}
	seen := make(map[string]struct{}, len(ids))
	result := make([]string, 0, len(ids))
	for _, id := range ids {
		id = strings.TrimSpace(id)
		if id == "" {
			continue
		}
		if _, ok := seen[id]; ok {
			continue
		}
		seen[id] = struct{}{}
		result = append(result, id)
	}
	sort.Strings(result)
	return result
}

func hasAnyTag(haystack, needles []string) bool {
	for _, have := range haystack {
		for _, needle := range needles {
			if have == needle {
				return true
			}
		}
	}
	return false
}
