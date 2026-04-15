package usage

import "time"

// UsageBucket captures cumulative usage within one bounded time window.
type UsageBucket struct {
	Start    time.Time       `json:"start"`
	End      time.Time       `json:"end"`
	Totals   Tokens          `json:"totals"`
	MCPProxy MCPProxyMetrics `json:"mcp_proxy"`
	Turns    int64           `json:"turns"`
}

// AgentTrend is the per-agent breakout within a workspace trend.
type AgentTrend struct {
	Agent                string          `json:"agent"`
	Available            bool            `json:"available"`
	LatestSessionID      string          `json:"latest_session_id,omitempty"`
	LatestTranscriptPath string          `json:"latest_transcript_path,omitempty"`
	Buckets              []UsageBucket   `json:"buckets,omitempty"`
	Total                Tokens          `json:"total"`
	MCPProxy             MCPProxyMetrics `json:"mcp_proxy"`
	LastActivity         *time.Time      `json:"last_activity,omitempty"`
	LatestTokens         Tokens          `json:"latest_tokens"`
	LatestMCPProxy       MCPProxyMetrics `json:"latest_mcp_proxy"`
	LatestModel          string          `json:"latest_model,omitempty"`
}

// WorkspaceTrend is the daemon-facing historical usage view for one workspace.
type WorkspaceTrend struct {
	Workspace         string          `json:"workspace"`
	Cwd               string          `json:"cwd,omitempty"`
	Available         bool            `json:"available"`
	Error             string          `json:"error,omitempty"`
	UnavailableReason string          `json:"unavailable_reason,omitempty"`
	WindowStart       time.Time       `json:"window_start"`
	WindowEnd         time.Time       `json:"window_end"`
	BucketMinutes     int             `json:"bucket_minutes"`
	Buckets           []UsageBucket   `json:"buckets,omitempty"`
	Total             Tokens          `json:"total"`
	MCPProxy          MCPProxyMetrics `json:"mcp_proxy"`
	LastActivity      *time.Time      `json:"last_activity,omitempty"`
	LatestTokens      Tokens          `json:"latest_tokens"`
	LatestMCPProxy    MCPProxyMetrics `json:"latest_mcp_proxy"`
	LatestModel       string          `json:"latest_model,omitempty"`
	Agents            []AgentTrend    `json:"agents,omitempty"`
}

// QueryWorkspaceTrend scans Claude transcript history for one workspace and
// aggregates recent usage into fixed-width time buckets.
func QueryWorkspaceTrend(workspace, cwd string, now time.Time, since, bucket time.Duration) WorkspaceTrend {
	trends, _ := QueryWorkspaceTrends([]WorkspaceBinding{{
		Name: workspace,
		Dir:  cwd,
	}}, now, since, bucket)
	if len(trends) > 0 {
		return trends[0]
	}

	if now.IsZero() {
		now = time.Now()
	}
	if since <= 0 {
		since = DefaultHistoryWindow
	}
	if bucket <= 0 {
		bucket = DefaultBucketSize
	}
	return WorkspaceTrend{
		Workspace:     workspace,
		Cwd:           cwd,
		WindowStart:   now.Add(-since).UTC(),
		WindowEnd:     now.UTC(),
		BucketMinutes: int(bucket / time.Minute),
		Error:         "workspace unavailable",
	}
}

// QueryWorkspaceTrends resolves multiple workspace requests together so same-cwd
// workspaces can be attributed via transcript workspace hints and shared session IDs.
func QueryWorkspaceTrends(bindings []WorkspaceBinding, now time.Time, since, bucket time.Duration) ([]WorkspaceTrend, error) {
	if now.IsZero() {
		now = time.Now()
	}
	if since <= 0 {
		since = DefaultHistoryWindow
	}
	if bucket <= 0 {
		bucket = DefaultBucketSize
	}

	resp, err := QueryHistory(bindings, HistoryQuery{
		Since:      now.Add(-since),
		Until:      now,
		BucketSize: bucket,
	})
	if err != nil {
		return nil, err
	}
	trends := make([]WorkspaceTrend, 0, len(resp.Workspaces))
	for _, ws := range resp.Workspaces {
		trend := WorkspaceTrend{
			Workspace:         ws.Workspace,
			Cwd:               ws.Dir,
			Available:         ws.Available,
			UnavailableReason: ws.UnavailableReason,
			WindowStart:       now.Add(-since).UTC(),
			WindowEnd:         now.UTC(),
			BucketMinutes:     int(bucket / time.Minute),
		}
		if !ws.Available {
			if ws.UnavailableReason != "" {
				trend.Error = ws.UnavailableReason
			}
			trends = append(trends, trend)
			continue
		}
		trend.Buckets = makeUsageBuckets(ws.RecentBuckets)
		trend.Total = sumBucketTotals(trend.Buckets)
		trend.MCPProxy = sumBucketMCPProxy(trend.Buckets)
		trend.LastActivity = ws.CurrentSnapshot.LastActivity
		trend.LatestTokens = ws.CurrentSnapshot.CurrentContext
		trend.LatestMCPProxy = ws.CurrentSnapshot.CurrentMCPProxy
		trend.LatestModel = ws.CurrentSnapshot.CurrentModel
		trend.Agents = makeAgentTrends(ws.Agents)
		trends = append(trends, trend)
	}
	return trends, nil
}

func makeUsageBuckets(buckets []Bucket) []UsageBucket {
	out := make([]UsageBucket, 0, len(buckets))
	for _, bucket := range buckets {
		out = append(out, UsageBucket{
			Start:    bucket.Start,
			End:      bucket.End,
			Totals:   bucket.Tokens,
			MCPProxy: bucket.MCPProxy,
			Turns:    bucket.Turns,
		})
	}
	return out
}

func makeAgentTrends(agents []AgentHistory) []AgentTrend {
	out := make([]AgentTrend, 0, len(agents))
	for _, agent := range agents {
		out = append(out, AgentTrend{
			Agent:                agent.Agent,
			Available:            agent.Available,
			LatestSessionID:      agent.LatestSessionID,
			LatestTranscriptPath: agent.LatestTranscript,
			Buckets:              makeUsageBuckets(agent.RecentBuckets),
			Total:                sumBucketTotals(makeUsageBuckets(agent.RecentBuckets)),
			MCPProxy:             sumBucketMCPProxy(makeUsageBuckets(agent.RecentBuckets)),
			LastActivity:         agent.CurrentSnapshot.LastActivity,
			LatestTokens:         agent.CurrentSnapshot.CurrentContext,
			LatestMCPProxy:       agent.CurrentSnapshot.CurrentMCPProxy,
			LatestModel:          agent.CurrentSnapshot.CurrentModel,
		})
	}
	return out
}

func sumBucketTotals(buckets []UsageBucket) Tokens {
	var total Tokens
	for _, bucket := range buckets {
		total = total.Add(bucket.Totals)
	}
	return total
}

func sumBucketMCPProxy(buckets []UsageBucket) MCPProxyMetrics {
	var total MCPProxyMetrics
	for _, bucket := range buckets {
		total = total.Add(bucket.MCPProxy)
	}
	return total
}
