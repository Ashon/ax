package cmd

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
)

func resetWatchCaches(t *testing.T) {
	t.Helper()
	historyCacheMu.Lock()
	historyCache = map[string]historyCacheEntry{}
	historyCacheMu.Unlock()

	tasksCacheMu.Lock()
	tasksCache = map[string]tasksCacheEntry{}
	tasksVersion = 0
	tasksCacheMu.Unlock()

	filterCacheMu.Lock()
	filterCache = filterCacheEntry{}
	filterCacheMu.Unlock()

	sidebarCacheMu.Lock()
	sidebarCache = sidebarCacheState{}
	sidebarCacheMu.Unlock()
}

func writeHistoryFile(t *testing.T, path string, entries []daemon.HistoryEntry) {
	t.Helper()
	f, err := os.Create(path)
	if err != nil {
		t.Fatalf("create history file: %v", err)
	}
	defer f.Close()
	enc := json.NewEncoder(f)
	for _, e := range entries {
		if err := enc.Encode(e); err != nil {
			t.Fatalf("encode history entry: %v", err)
		}
	}
}

func TestReadHistoryFileReusesCachedSliceWhileUnchanged(t *testing.T) {
	resetWatchCaches(t)
	dir := t.TempDir()
	path := filepath.Join(dir, "history.jsonl")

	writeHistoryFile(t, path, []daemon.HistoryEntry{
		{From: "a", To: "b", Content: "hi", Timestamp: time.Unix(1000, 0)},
		{From: "b", To: "a", Content: "yo", Timestamp: time.Unix(1001, 0)},
	})

	first := readHistoryFile(path, 50)
	second := readHistoryFile(path, 50)
	if len(first) != 2 {
		t.Fatalf("expected 2 entries, got %d", len(first))
	}
	// Cache hit must return the same backing slice header.
	if len(first) > 0 && &first[0] != &second[0] {
		t.Fatalf("expected cached slice to be reused")
	}

	// Changing mtime/size should invalidate the cache.
	time.Sleep(10 * time.Millisecond) // ensure mtime tick
	writeHistoryFile(t, path, []daemon.HistoryEntry{
		{From: "a", To: "b", Content: "hi", Timestamp: time.Unix(1000, 0)},
		{From: "b", To: "a", Content: "yo", Timestamp: time.Unix(1001, 0)},
		{From: "a", To: "c", Content: "new", Timestamp: time.Unix(1002, 0)},
	})
	if err := os.Chtimes(path, time.Now(), time.Now()); err != nil {
		t.Fatalf("chtimes: %v", err)
	}
	third := readHistoryFile(path, 50)
	if len(third) != 3 {
		t.Fatalf("expected cache invalidation, got %d entries", len(third))
	}
}

func TestReadHistoryFileKeysOnMaxEntries(t *testing.T) {
	resetWatchCaches(t)
	dir := t.TempDir()
	path := filepath.Join(dir, "history.jsonl")

	var entries []daemon.HistoryEntry
	for i := 0; i < 5; i++ {
		entries = append(entries, daemon.HistoryEntry{
			From:      "a",
			To:        "b",
			Content:   strconv.Itoa(i),
			Timestamp: time.Unix(int64(1000+i), 0),
		})
	}
	writeHistoryFile(t, path, entries)

	got2 := readHistoryFile(path, 2)
	if len(got2) != 2 {
		t.Fatalf("expected 2 entries, got %d", len(got2))
	}
	got5 := readHistoryFile(path, 5)
	if len(got5) != 5 {
		t.Fatalf("expected 5 entries with different limit, got %d", len(got5))
	}
}

func writeTasksFile(t *testing.T, path string, tasks []types.Task) {
	t.Helper()
	data, err := json.Marshal(tasks)
	if err != nil {
		t.Fatalf("marshal tasks: %v", err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatalf("write tasks: %v", err)
	}
}

func TestReadTasksFileCacheAndVersion(t *testing.T) {
	resetWatchCaches(t)
	dir := t.TempDir()
	path := filepath.Join(dir, "tasks.json")

	writeTasksFile(t, path, []types.Task{
		{ID: "t1", Status: types.TaskPending, UpdatedAt: time.Unix(1000, 0)},
		{ID: "t2", Status: types.TaskInProgress, UpdatedAt: time.Unix(1001, 0)},
	})

	first := readTasksFile(path)
	v1 := tasksCacheVersionFor(path)
	second := readTasksFile(path)
	v2 := tasksCacheVersionFor(path)
	if v1 == 0 || v1 != v2 {
		t.Fatalf("expected stable version on cache hit, got %d then %d", v1, v2)
	}
	if len(first) > 0 && &first[0] != &second[0] {
		t.Fatalf("expected cached tasks slice to be reused on hit")
	}

	time.Sleep(10 * time.Millisecond)
	writeTasksFile(t, path, []types.Task{
		{ID: "t1", Status: types.TaskPending, UpdatedAt: time.Unix(1000, 0)},
	})
	if err := os.Chtimes(path, time.Now(), time.Now()); err != nil {
		t.Fatalf("chtimes: %v", err)
	}
	third := readTasksFile(path)
	v3 := tasksCacheVersionFor(path)
	if v3 <= v2 {
		t.Fatalf("expected version bump on file change, got %d -> %d", v2, v3)
	}
	if len(third) != 1 {
		t.Fatalf("expected reload, got %d tasks", len(third))
	}
}

func TestFilterTasksCachedReusesResultOnStableVersion(t *testing.T) {
	resetWatchCaches(t)
	tasks := []types.Task{
		{ID: "t1", Status: types.TaskPending, UpdatedAt: time.Unix(1000, 0)},
		{ID: "t2", Status: types.TaskCompleted, UpdatedAt: time.Unix(1001, 0)},
		{ID: "t3", Status: types.TaskInProgress, UpdatedAt: time.Unix(1002, 0)},
	}

	first := filterTasksCached(tasks, taskFilterActive, 42)
	second := filterTasksCached(tasks, taskFilterActive, 42)
	if len(first) != 2 {
		t.Fatalf("expected 2 active tasks, got %d", len(first))
	}
	if &first[0] != &second[0] {
		t.Fatalf("expected filter cache hit to reuse slice")
	}

	// Different filter must recompute.
	third := filterTasksCached(tasks, taskFilterDone, 42)
	if len(third) != 1 {
		t.Fatalf("expected 1 done task, got %d", len(third))
	}

	// Back to active with same version should still hit... but the filter
	// cache keeps a single slot, so this is allowed to recompute. We only
	// assert the result is correct.
	back := filterTasksCached(tasks, taskFilterActive, 42)
	if len(back) != 2 {
		t.Fatalf("expected 2 active tasks on recompute, got %d", len(back))
	}

	// Bumping version invalidates.
	bumped := filterTasksCached(tasks, taskFilterActive, 43)
	if len(bumped) != 2 {
		t.Fatalf("expected 2 active tasks after version bump, got %d", len(bumped))
	}
}

func TestBuildSidebarEntriesCachedReusesOnStableInputs(t *testing.T) {
	resetWatchCaches(t)
	sessions := []tmux.SessionInfo{
		{Name: "ws:alpha", Workspace: "alpha"},
		{Name: "ws:beta", Workspace: "beta"},
	}
	first := buildSidebarEntriesCached(sessions)
	second := buildSidebarEntriesCached(sessions)
	if len(first) == 0 {
		t.Fatalf("expected non-empty sidebar entries")
	}
	if &first[0] != &second[0] {
		t.Fatalf("expected sidebar cache hit to reuse slice")
	}

	changed := []tmux.SessionInfo{
		{Name: "ws:alpha", Workspace: "alpha"},
	}
	third := buildSidebarEntriesCached(changed)
	if len(third) == 0 {
		t.Fatalf("expected recompute on session change")
	}
	if len(first) > 0 && len(third) > 0 && &first[0] == &third[0] {
		t.Fatalf("expected fresh slice after invalidation")
	}
}

func TestClampIndex(t *testing.T) {
	cases := []struct {
		name    string
		current int
		n       int
		want    int
	}{
		{"empty returns 0", 3, 0, 0},
		{"negative clamps to 0", -1, 5, 0},
		{"past end clamps to last", 10, 5, 4},
		{"in range preserved", 2, 5, 2},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := clampIndex(tc.current, tc.n); got != tc.want {
				t.Fatalf("clampIndex(%d, %d) = %d, want %d", tc.current, tc.n, got, tc.want)
			}
		})
	}
}
