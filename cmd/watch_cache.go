package cmd

import (
	"os"
	"strings"
	"sync"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
)

// The watch TUI updates at 60 FPS, which makes several per-tick operations
// expensive: full history/tasks file reads + JSON decode, repeated task
// filtering + sorting, sidebar tree rebuilds, and tmux capture-pane execs.
//
// The helpers in this file wrap those hot paths with lightweight caches
// keyed on cheap invalidation signals (file mtime/size, session list
// signature, data version counters) so that unchanged inputs reuse the
// previous result instead of re-running the work every frame.

// ---------- history file cache ----------

type historyCacheEntry struct {
	modTime    time.Time
	size       int64
	maxEntries int
	entries    []daemon.HistoryEntry
}

var (
	historyCacheMu sync.Mutex
	historyCache   = map[string]historyCacheEntry{}
)

// readHistoryFile returns the newest maxEntries entries of the history file
// at path. Results are cached per (path, maxEntries) and reused while the
// file's mtime/size are unchanged.
func readHistoryFile(path string, maxEntries int) []daemon.HistoryEntry {
	info, err := os.Stat(path)
	if err != nil {
		historyCacheMu.Lock()
		delete(historyCache, path)
		historyCacheMu.Unlock()
		return nil
	}

	historyCacheMu.Lock()
	if e, ok := historyCache[path]; ok &&
		e.maxEntries == maxEntries &&
		e.size == info.Size() &&
		e.modTime.Equal(info.ModTime()) {
		entries := e.entries
		historyCacheMu.Unlock()
		return entries
	}
	historyCacheMu.Unlock()

	entries := readHistoryFileUncached(path, maxEntries)

	historyCacheMu.Lock()
	historyCache[path] = historyCacheEntry{
		modTime:    info.ModTime(),
		size:       info.Size(),
		maxEntries: maxEntries,
		entries:    entries,
	}
	historyCacheMu.Unlock()
	return entries
}

// ---------- tasks file cache ----------

type tasksCacheEntry struct {
	modTime time.Time
	size    int64
	version uint64
	tasks   []types.Task
}

var (
	tasksCacheMu sync.Mutex
	tasksCache   = map[string]tasksCacheEntry{}
	tasksVersion uint64 // monotonic, bumped whenever any cached tasks file changes
)

// readTasksFile parses and sorts the tasks file at path. Results are cached
// and reused while the file's mtime/size are unchanged. Each miss bumps a
// monotonic version counter which downstream helpers (filterTasks cache,
// etc.) use to detect staleness cheaply.
func readTasksFile(path string) []types.Task {
	info, err := os.Stat(path)
	if err != nil {
		tasksCacheMu.Lock()
		delete(tasksCache, path)
		tasksCacheMu.Unlock()
		return nil
	}

	tasksCacheMu.Lock()
	if e, ok := tasksCache[path]; ok &&
		e.size == info.Size() &&
		e.modTime.Equal(info.ModTime()) {
		tasks := e.tasks
		tasksCacheMu.Unlock()
		return tasks
	}
	tasksCacheMu.Unlock()

	tasks := readTasksFileUncached(path)

	tasksCacheMu.Lock()
	tasksVersion++
	tasksCache[path] = tasksCacheEntry{
		modTime: info.ModTime(),
		size:    info.Size(),
		version: tasksVersion,
		tasks:   tasks,
	}
	tasksCacheMu.Unlock()
	return tasks
}

// tasksCacheVersionFor returns the version counter associated with the
// currently cached tasks for path, or 0 if nothing is cached.
func tasksCacheVersionFor(path string) uint64 {
	tasksCacheMu.Lock()
	defer tasksCacheMu.Unlock()
	if e, ok := tasksCache[path]; ok {
		return e.version
	}
	return 0
}

// ---------- filterTasks cache ----------

type filterCacheEntry struct {
	version uint64
	length  int
	filter  taskFilterMode
	result  []types.Task
}

var (
	filterCacheMu sync.Mutex
	filterCache   filterCacheEntry
)

// filterTasksCached memoizes filterTasks. It is safe to call from the hot
// rendering path because the underlying tasks slice is itself cached by
// readTasksFile, so identical inputs produce a cheap cache hit.
//
// Invalidation uses the tasks-file version counter when available, falling
// back to a length-based heuristic for tasks slices that did not come from
// readTasksFile.
func filterTasksCached(tasks []types.Task, filter taskFilterMode, version uint64) []types.Task {
	filterCacheMu.Lock()
	if filterCache.result != nil &&
		filterCache.filter == filter &&
		filterCache.length == len(tasks) &&
		(version != 0 && filterCache.version == version) {
		result := filterCache.result
		filterCacheMu.Unlock()
		return result
	}
	filterCacheMu.Unlock()

	result := filterTasks(tasks, filter)

	filterCacheMu.Lock()
	filterCache = filterCacheEntry{
		version: version,
		length:  len(tasks),
		filter:  filter,
		result:  result,
	}
	filterCacheMu.Unlock()
	return result
}

// ---------- sidebar cache ----------

type sidebarCacheState struct {
	signature  string
	cfgPath    string
	cfgModTime time.Time
	entries    []sidebarEntry
}

var (
	sidebarCacheMu sync.Mutex
	sidebarCache   sidebarCacheState
)

// buildSidebarEntriesCached wraps buildSidebarEntries. The sidebar only
// changes when the set of running sessions or the project-tree config file
// changes, so we reuse the previous entries while those signals are stable.
func buildSidebarEntriesCached(sessions []tmux.SessionInfo) []sidebarEntry {
	sig := sidebarSessionSignature(sessions)

	cfgPath := ""
	var cfgMod time.Time
	if p, err := resolveConfigPath(); err == nil {
		cfgPath = p
		if info, err := os.Stat(p); err == nil {
			cfgMod = info.ModTime()
		}
	}

	sidebarCacheMu.Lock()
	if sidebarCache.entries != nil &&
		sidebarCache.signature == sig &&
		sidebarCache.cfgPath == cfgPath &&
		sidebarCache.cfgModTime.Equal(cfgMod) {
		entries := sidebarCache.entries
		sidebarCacheMu.Unlock()
		return entries
	}
	sidebarCacheMu.Unlock()

	entries := buildSidebarEntries(sessions)

	sidebarCacheMu.Lock()
	sidebarCache = sidebarCacheState{
		signature:  sig,
		cfgPath:    cfgPath,
		cfgModTime: cfgMod,
		entries:    entries,
	}
	sidebarCacheMu.Unlock()
	return entries
}

func sidebarSessionSignature(sessions []tmux.SessionInfo) string {
	var b strings.Builder
	b.Grow(len(sessions) * 24)
	for _, s := range sessions {
		b.WriteString(s.Workspace)
		b.WriteByte('\x00')
		b.WriteString(s.Name)
		b.WriteByte('\n')
	}
	return b.String()
}

// ---------- capture-pane throttle ----------

type captureCacheEntry struct {
	content string
	fetched time.Time
}

var (
	captureCacheMu sync.Mutex
	captureCache   = map[string]captureCacheEntry{}
)

// capturePaneThrottled returns the tmux capture-pane output for sessionName,
// reusing a previous result whose age is below maxAge. Passing maxAge <= 0
// bypasses the cache and always fetches fresh content. The cache is
// populated on every call (including bypass calls) so cache hits remain
// consistent across callers.
func capturePaneThrottled(sessionName string, maxAge time.Duration) string {
	if maxAge > 0 {
		captureCacheMu.Lock()
		if e, ok := captureCache[sessionName]; ok && time.Since(e.fetched) < maxAge {
			content := e.content
			captureCacheMu.Unlock()
			return content
		}
		captureCacheMu.Unlock()
	}

	content := capturePane(sessionName)

	captureCacheMu.Lock()
	captureCache[sessionName] = captureCacheEntry{
		content: content,
		fetched: time.Now(),
	}
	captureCacheMu.Unlock()
	return content
}
