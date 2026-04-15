package usage

import (
	"sort"
	"time"
)

// Aggregator accumulates parsed records into a WorkspaceUsage snapshot.
// One Aggregator corresponds to one transcript file (one session). When
// the session changes, callers should Reset it.
type Aggregator struct {
	sessionID    string
	sessionStart *time.Time
	lastActivity *time.Time

	cumulative    Tokens
	cumulativeMCP MCPProxyMetrics
	byModel       map[string]*ModelTotals
	currentTokens Tokens
	currentMCP    MCPProxyMetrics
	currentModel  string
	turns         int64

	parseErrors int64
}

// NewAggregator returns an empty aggregator.
func NewAggregator() *Aggregator {
	return &Aggregator{byModel: map[string]*ModelTotals{}}
}

// Reset clears accumulated state (used when the active session file changes).
func (a *Aggregator) Reset() {
	*a = Aggregator{byModel: map[string]*ModelTotals{}}
}

// SessionID returns the most recent session id observed, or "".
func (a *Aggregator) SessionID() string { return a.sessionID }

// ParseErrors returns the count of malformed lines encountered.
func (a *Aggregator) ParseErrors() int64 { return a.parseErrors }

// Turns returns the number of usage-bearing turns ingested.
func (a *Aggregator) Turns() int64 { return a.turns }

// Ingest folds a parsed record into the running totals. Returns true if
// the record carried a usage payload and was counted.
func (a *Aggregator) Ingest(r parsedRecord) bool {
	if r.SessionID != "" {
		a.sessionID = r.SessionID
	}
	if !r.Timestamp.IsZero() {
		ts := r.Timestamp
		if a.sessionStart == nil {
			tsCopy := ts
			a.sessionStart = &tsCopy
		}
		tsCopy := ts
		a.lastActivity = &tsCopy
	}
	if r.MCPProxy.Total > 0 {
		a.cumulativeMCP = a.cumulativeMCP.Add(r.MCPProxy)
		a.currentMCP = r.MCPProxy
	}
	if !r.HasUsage {
		return false
	}
	a.cumulative = a.cumulative.Add(r.Tokens)
	a.currentTokens = r.Tokens
	a.currentModel = r.Model
	a.turns++
	mt, ok := a.byModel[r.Model]
	if !ok {
		mt = &ModelTotals{Model: r.Model}
		a.byModel[r.Model] = mt
	}
	mt.Totals = mt.Totals.Add(r.Tokens)
	mt.Turns++
	return true
}

// IngestLine parses and ingests a single jsonl line. Malformed lines
// bump ParseErrors and return false. Returns true on usage-bearing lines.
func (a *Aggregator) IngestLine(line []byte) bool {
	rec, err := parseLine(line)
	if err != nil {
		a.parseErrors++
		return false
	}
	return a.Ingest(rec)
}

// Snapshot materializes the current state into a WorkspaceUsage value.
// ByModel is sorted by model name for deterministic output. Available is
// set true when at least one usage-bearing record or any sessionId has
// been observed.
func (a *Aggregator) Snapshot(workspace, transcriptPath string) WorkspaceUsage {
	models := make([]ModelTotals, 0, len(a.byModel))
	for _, m := range a.byModel {
		models = append(models, *m)
	}
	sort.Slice(models, func(i, j int) bool { return models[i].Model < models[j].Model })
	return WorkspaceUsage{
		Workspace:        workspace,
		TranscriptPath:   transcriptPath,
		SessionID:        a.sessionID,
		SessionStart:     a.sessionStart,
		LastActivity:     a.lastActivity,
		CumulativeTotals: a.cumulative,
		CumulativeMCP:    a.cumulativeMCP,
		ByModel:          models,
		CurrentContext:   a.currentTokens,
		CurrentMCP:       a.currentMCP,
		CurrentModel:     a.currentModel,
		Turns:            a.turns,
		Available:        a.turns > 0 || a.sessionID != "",
	}
}
