package usage

import (
	"bufio"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"
)

const (
	DefaultHistoryWindow = 3 * time.Hour
	DefaultBucketSize    = 5 * time.Minute
)

var workspaceHintPattern = regexp.MustCompile(`You are the "([^"]+)" workspace agent in an ax multi-agent environment\.`)

// WorkspaceBinding is the daemon's view of one active ax workspace.
type WorkspaceBinding struct {
	Name string `json:"name"`
	Dir  string `json:"dir,omitempty"`
}

// HistoryQuery describes a bounded usage-history lookup.
type HistoryQuery struct {
	Workspace  string
	Since      time.Time
	Until      time.Time
	BucketSize time.Duration
}

// HistoryResponse is the daemon/MCP response shape for recent usage trends.
type HistoryResponse struct {
	Since         time.Time          `json:"since"`
	Until         time.Time          `json:"until"`
	BucketMinutes int                `json:"bucket_minutes"`
	Workspaces    []WorkspaceHistory `json:"workspaces"`
}

// WorkspaceHistory is the workspace-level rollup the CLI can render directly.
type WorkspaceHistory struct {
	Workspace         string          `json:"workspace"`
	Dir               string          `json:"dir,omitempty"`
	Available         bool            `json:"available"`
	UnavailableReason string          `json:"unavailable_reason,omitempty"`
	CurrentSnapshot   CurrentSnapshot `json:"current_snapshot"`
	RecentBuckets     []Bucket        `json:"recent_buckets,omitempty"`
	Agents            []AgentHistory  `json:"agents,omitempty"`
}

// AgentHistory is the per-agent breakout within a workspace.
type AgentHistory struct {
	Agent              string          `json:"agent"`
	Available          bool            `json:"available"`
	LatestSessionID    string          `json:"latest_session_id,omitempty"`
	LatestTranscript   string          `json:"latest_transcript_path,omitempty"`
	CurrentSnapshot    CurrentSnapshot `json:"current_snapshot"`
	RecentBuckets      []Bucket        `json:"recent_buckets,omitempty"`
	SourceTranscriptCt int             `json:"source_transcript_count,omitempty"`
}

// CurrentSnapshot captures the latest session snapshot for an agent, or an
// aggregate of the latest agent snapshots at workspace scope.
type CurrentSnapshot struct {
	LastActivity       *time.Time      `json:"last_activity,omitempty"`
	CurrentContext     Tokens          `json:"current_context"`
	CurrentTotal       int64           `json:"current_total"`
	CurrentMCPProxy    MCPProxyMetrics `json:"current_mcp_proxy"`
	CurrentModel       string          `json:"current_model,omitempty"`
	CumulativeTotals   Tokens          `json:"cumulative_totals"`
	CumulativeTotal    int64           `json:"cumulative_total"`
	CumulativeMCPProxy MCPProxyMetrics `json:"cumulative_mcp_proxy"`
	Turns              int64           `json:"turns"`
}

// Bucket is one fixed-size historical usage bucket.
type Bucket struct {
	Start    time.Time       `json:"start"`
	End      time.Time       `json:"end"`
	Tokens   Tokens          `json:"tokens"`
	Total    int64           `json:"total"`
	MCPProxy MCPProxyMetrics `json:"mcp_proxy"`
	Turns    int64           `json:"turns"`
}

type transcriptSeries struct {
	cwd           string
	sessionID     string
	agent         string
	workspaceHint string
	transcript    string
	current       CurrentSnapshot
	buckets       []Bucket
}

type workspaceAssignment struct {
	series []*transcriptSeries
}

type dirState struct {
	projectDir       string
	projectExists    bool
	transcriptsFound bool
}

// QueryHistory scans local Claude transcripts and returns recent usage trends
// for the supplied active workspaces.
func QueryHistory(bindings []WorkspaceBinding, q HistoryQuery) (HistoryResponse, error) {
	q = normalizeHistoryQuery(q)

	bindings = normalizeBindings(bindings, q.Workspace)
	resp := HistoryResponse{
		Since:         q.Since.UTC(),
		Until:         q.Until.UTC(),
		BucketMinutes: int(q.BucketSize / time.Minute),
		Workspaces:    make([]WorkspaceHistory, 0, len(bindings)),
	}
	if len(bindings) == 0 {
		return resp, nil
	}

	states, series, err := scanBindings(bindings, q)
	if err != nil {
		return resp, err
	}
	assignments := assignSeries(bindings, series)

	for _, binding := range bindings {
		ws := WorkspaceHistory{
			Workspace: binding.Name,
			Dir:       binding.Dir,
		}
		state := states[cleanPath(binding.Dir)]
		assigned := assignments[binding.Name]
		if len(assigned.series) == 0 {
			ws.Available = false
			switch {
			case cleanPath(binding.Dir) == "":
				ws.UnavailableReason = "missing_workspace_dir"
			case !state.projectExists:
				ws.UnavailableReason = "no_project_transcripts"
			case !state.transcriptsFound:
				ws.UnavailableReason = "no_transcripts"
			default:
				ws.UnavailableReason = "workspace_unattributed"
			}
			resp.Workspaces = append(resp.Workspaces, ws)
			continue
		}

		agents := buildAgentHistories(assigned.series, q.BucketSize)
		ws.Available = true
		ws.Agents = agents
		ws.RecentBuckets = aggregateBuckets(agentBuckets(agents), q.BucketSize)
		ws.CurrentSnapshot = aggregateSnapshots(agentSnapshots(agents))
		resp.Workspaces = append(resp.Workspaces, ws)
	}

	sort.Slice(resp.Workspaces, func(i, j int) bool {
		return resp.Workspaces[i].Workspace < resp.Workspaces[j].Workspace
	})
	return resp, nil
}

func normalizeHistoryQuery(q HistoryQuery) HistoryQuery {
	if q.Until.IsZero() {
		q.Until = time.Now()
	}
	if q.BucketSize <= 0 {
		q.BucketSize = DefaultBucketSize
	}
	if q.Since.IsZero() || !q.Since.Before(q.Until) {
		q.Since = q.Until.Add(-DefaultHistoryWindow)
	}
	q.Until = q.Until.UTC()
	q.Since = q.Since.UTC()
	return q
}

func normalizeBindings(bindings []WorkspaceBinding, only string) []WorkspaceBinding {
	filtered := make([]WorkspaceBinding, 0, len(bindings))
	seen := map[string]struct{}{}
	for _, binding := range bindings {
		name := strings.TrimSpace(binding.Name)
		if name == "" {
			continue
		}
		if only != "" && only != name {
			continue
		}
		if _, ok := seen[name]; ok {
			continue
		}
		seen[name] = struct{}{}
		binding.Name = name
		binding.Dir = cleanPath(binding.Dir)
		filtered = append(filtered, binding)
	}
	sort.Slice(filtered, func(i, j int) bool { return filtered[i].Name < filtered[j].Name })
	return filtered
}

func scanBindings(bindings []WorkspaceBinding, q HistoryQuery) (map[string]dirState, []*transcriptSeries, error) {
	states := make(map[string]dirState, len(bindings))
	seenDirs := map[string]struct{}{}
	series := make([]*transcriptSeries, 0, 16)
	for _, binding := range bindings {
		dir := cleanPath(binding.Dir)
		if _, ok := seenDirs[dir]; ok {
			continue
		}
		seenDirs[dir] = struct{}{}
		state := dirState{}
		states[dir] = state
		if dir == "" {
			continue
		}

		projectDir, err := ProjectPath(dir)
		if err != nil {
			return nil, nil, err
		}
		state.projectDir = projectDir
		if _, err := os.Stat(projectDir); err != nil {
			if errors.Is(err, os.ErrNotExist) {
				states[dir] = state
				continue
			}
			return nil, nil, err
		}
		state.projectExists = true

		paths, err := discoverTranscripts(projectDir)
		if err != nil {
			return nil, nil, err
		}
		if len(paths) == 0 {
			states[dir] = state
			continue
		}
		state.transcriptsFound = true
		states[dir] = state

		for _, path := range paths {
			s, err := scanTranscript(path, q)
			if err != nil {
				return nil, nil, err
			}
			if s != nil {
				series = append(series, s)
			}
		}
	}
	return states, series, nil
}

func discoverTranscripts(projectDir string) ([]string, error) {
	paths := make([]string, 0, 16)
	err := filepath.WalkDir(projectDir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			return nil
		}
		if filepath.Ext(path) != ".jsonl" {
			return nil
		}
		paths = append(paths, path)
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Strings(paths)
	return paths, nil
}

func scanTranscript(path string, q HistoryQuery) (*transcriptSeries, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1024*1024), 4*1024*1024)
	agg := NewAggregator()
	buckets := map[time.Time]*Bucket{}
	series := &transcriptSeries{
		agent:      agentFromTranscriptPath(path),
		transcript: path,
	}

	for scanner.Scan() {
		raw, err := decodeRawRecord(scanner.Bytes())
		if err != nil {
			continue
		}
		if series.sessionID == "" && raw.SessionID != "" {
			series.sessionID = raw.SessionID
		}
		if series.cwd == "" && raw.Cwd != "" {
			series.cwd = cleanPath(raw.Cwd)
		}
		if raw.AgentID != "" {
			series.agent = raw.AgentID
		}
		if series.workspaceHint == "" {
			series.workspaceHint = workspaceHintFromAttachment(raw.Attachment)
		}

		rec := parsedRecordFromRaw(raw)
		counted := agg.Ingest(rec)
		if rec.Timestamp.IsZero() || rec.Timestamp.Before(q.Since) || !rec.Timestamp.Before(q.Until) {
			continue
		}
		if !counted && rec.MCPProxy.Total == 0 {
			continue
		}
		start := rec.Timestamp.UTC().Truncate(q.BucketSize)
		b := buckets[start]
		if b == nil {
			b = &Bucket{
				Start: start,
				End:   start.Add(q.BucketSize),
			}
			buckets[start] = b
		}
		if counted {
			b.Tokens = b.Tokens.Add(rec.Tokens)
			b.Total += rec.Tokens.Total()
			b.Turns++
		}
		if rec.MCPProxy.Total > 0 {
			b.MCPProxy = b.MCPProxy.Add(rec.MCPProxy)
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}

	if series.agent == "" {
		series.agent = "main"
	}
	snap := agg.Snapshot("", path)
	if !snap.Available && series.workspaceHint == "" && series.sessionID == "" && series.cwd == "" {
		return nil, nil
	}
	series.current = CurrentSnapshot{
		LastActivity:       snap.LastActivity,
		CurrentContext:     snap.CurrentContext,
		CurrentTotal:       snap.CurrentContext.Total(),
		CurrentMCPProxy:    snap.CurrentMCP,
		CurrentModel:       snap.CurrentModel,
		CumulativeTotals:   snap.CumulativeTotals,
		CumulativeTotal:    snap.CumulativeTotals.Total(),
		CumulativeMCPProxy: snap.CumulativeMCP,
		Turns:              snap.Turns,
	}
	series.buckets = sortBuckets(buckets)
	return series, nil
}

func workspaceHintFromAttachment(att *rawAttachment) string {
	if att == nil || att.Type != "mcp_instructions_delta" {
		return ""
	}
	for _, block := range att.AddedBlocks {
		m := workspaceHintPattern.FindStringSubmatch(block)
		if len(m) == 2 {
			return strings.TrimSpace(m[1])
		}
	}
	return ""
}

func assignSeries(bindings []WorkspaceBinding, series []*transcriptSeries) map[string]workspaceAssignment {
	assignments := make(map[string]workspaceAssignment, len(bindings))
	bindingByName := make(map[string]WorkspaceBinding, len(bindings))
	uniqueByDir := make(map[string]string, len(bindings))
	for _, binding := range bindings {
		bindingByName[binding.Name] = binding
		dir := cleanPath(binding.Dir)
		if dir == "" {
			continue
		}
		if existing, ok := uniqueByDir[dir]; ok && existing != binding.Name {
			uniqueByDir[dir] = ""
			continue
		}
		uniqueByDir[dir] = binding.Name
	}

	sessionWorkspace := map[string]string{}
	for _, s := range series {
		if s.workspaceHint == "" {
			continue
		}
		binding, ok := bindingByName[s.workspaceHint]
		if !ok {
			continue
		}
		if binding.Dir != "" && s.cwd != "" && cleanPath(binding.Dir) != cleanPath(s.cwd) {
			continue
		}
		assignments[binding.Name] = workspaceAssignment{
			series: append(assignments[binding.Name].series, s),
		}
		if s.sessionID != "" {
			sessionWorkspace[s.sessionID] = binding.Name
		}
	}

	for _, s := range series {
		if s.workspaceHint != "" {
			continue
		}
		workspace := ""
		if s.sessionID != "" {
			workspace = sessionWorkspace[s.sessionID]
		}
		if workspace == "" && s.cwd != "" {
			workspace = uniqueByDir[cleanPath(s.cwd)]
		}
		if workspace == "" {
			continue
		}
		assignments[workspace] = workspaceAssignment{
			series: append(assignments[workspace].series, s),
		}
	}

	return assignments
}

func buildAgentHistories(series []*transcriptSeries, bucketSize time.Duration) []AgentHistory {
	type agentAccumulator struct {
		current       CurrentSnapshot
		buckets       []Bucket
		latestSession string
		latestPath    string
		sourceCount   int
	}

	acc := map[string]*agentAccumulator{}
	for _, s := range series {
		entry := acc[s.agent]
		if entry == nil {
			entry = &agentAccumulator{}
			acc[s.agent] = entry
		}
		entry.buckets = aggregateBuckets([][]Bucket{entry.buckets, s.buckets}, bucketSize)
		entry.sourceCount++
		if newerSnapshot(s.current.LastActivity, entry.current.LastActivity) {
			entry.current = s.current
			entry.latestSession = s.sessionID
			entry.latestPath = s.transcript
		}
	}

	agents := make([]AgentHistory, 0, len(acc))
	for agent, entry := range acc {
		agents = append(agents, AgentHistory{
			Agent:              agent,
			Available:          true,
			LatestSessionID:    entry.latestSession,
			LatestTranscript:   entry.latestPath,
			CurrentSnapshot:    entry.current,
			RecentBuckets:      entry.buckets,
			SourceTranscriptCt: entry.sourceCount,
		})
	}
	sort.Slice(agents, func(i, j int) bool {
		if agents[i].Agent == "main" {
			return true
		}
		if agents[j].Agent == "main" {
			return false
		}
		return agents[i].Agent < agents[j].Agent
	})
	return agents
}

func agentBuckets(agents []AgentHistory) [][]Bucket {
	buckets := make([][]Bucket, 0, len(agents))
	for _, agent := range agents {
		buckets = append(buckets, agent.RecentBuckets)
	}
	return buckets
}

func agentSnapshots(agents []AgentHistory) []CurrentSnapshot {
	snaps := make([]CurrentSnapshot, 0, len(agents))
	for _, agent := range agents {
		snaps = append(snaps, agent.CurrentSnapshot)
	}
	return snaps
}

func aggregateBuckets(all [][]Bucket, bucketSize time.Duration) []Bucket {
	byStart := map[time.Time]*Bucket{}
	for _, buckets := range all {
		for _, bucket := range buckets {
			entry := byStart[bucket.Start]
			if entry == nil {
				copy := bucket
				if copy.End.IsZero() {
					copy.End = copy.Start.Add(bucketSize)
				}
				byStart[bucket.Start] = &copy
				continue
			}
			entry.Tokens = entry.Tokens.Add(bucket.Tokens)
			entry.Total += bucket.Total
			entry.MCPProxy = entry.MCPProxy.Add(bucket.MCPProxy)
			entry.Turns += bucket.Turns
		}
	}
	return sortBuckets(byStart)
}

func aggregateSnapshots(snaps []CurrentSnapshot) CurrentSnapshot {
	var current CurrentSnapshot
	for _, snap := range snaps {
		current.CurrentContext = current.CurrentContext.Add(snap.CurrentContext)
		current.CurrentTotal += snap.CurrentTotal
		current.CurrentMCPProxy = current.CurrentMCPProxy.Add(snap.CurrentMCPProxy)
		current.CumulativeTotals = current.CumulativeTotals.Add(snap.CumulativeTotals)
		current.CumulativeTotal += snap.CumulativeTotal
		current.CumulativeMCPProxy = current.CumulativeMCPProxy.Add(snap.CumulativeMCPProxy)
		current.Turns += snap.Turns
		if newerSnapshot(snap.LastActivity, current.LastActivity) {
			current.LastActivity = snap.LastActivity
			current.CurrentModel = snap.CurrentModel
		}
	}
	return current
}

func sortBuckets(byStart map[time.Time]*Bucket) []Bucket {
	buckets := make([]Bucket, 0, len(byStart))
	for _, bucket := range byStart {
		buckets = append(buckets, *bucket)
	}
	sort.Slice(buckets, func(i, j int) bool {
		return buckets[i].Start.Before(buckets[j].Start)
	})
	return buckets
}

func agentFromTranscriptPath(path string) string {
	base := filepath.Base(path)
	if strings.HasPrefix(base, "agent-") && strings.HasSuffix(base, ".jsonl") {
		return strings.TrimSuffix(strings.TrimPrefix(base, "agent-"), ".jsonl")
	}
	return "main"
}

func cleanPath(path string) string {
	if strings.TrimSpace(path) == "" {
		return ""
	}
	return filepath.Clean(path)
}

func newerSnapshot(candidate, current *time.Time) bool {
	if candidate == nil {
		return false
	}
	if current == nil {
		return true
	}
	return candidate.After(*current)
}
