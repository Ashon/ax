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
	requests    map[string]*assistantRequestState
}

type assistantRequestState struct {
	model    string
	tokens   Tokens
	usesMCP  bool
	mcpProxy MCPProxyMetrics
}

type ingestResult struct {
	UsageObserved bool
	TokensDelta   Tokens
	MCPDelta      MCPProxyMetrics
	TurnDelta     int64
}

// NewAggregator returns an empty aggregator.
func NewAggregator() *Aggregator {
	return &Aggregator{
		byModel:  map[string]*ModelTotals{},
		requests: map[string]*assistantRequestState{},
	}
}

// Reset clears accumulated state (used when the active session file changes).
func (a *Aggregator) Reset() {
	*a = Aggregator{
		byModel:  map[string]*ModelTotals{},
		requests: map[string]*assistantRequestState{},
	}
}

// SessionID returns the most recent session id observed, or "".
func (a *Aggregator) SessionID() string { return a.sessionID }

// ParseErrors returns the count of malformed lines encountered.
func (a *Aggregator) ParseErrors() int64 { return a.parseErrors }

// Turns returns the number of usage-bearing turns ingested.
func (a *Aggregator) Turns() int64 { return a.turns }

// Ingest folds a parsed record into the running totals and returns the
// effective delta after request-level de-duplication.
func (a *Aggregator) Ingest(r parsedRecord) ingestResult {
	result := ingestResult{UsageObserved: r.HasUsage}
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
	if !r.HasUsage {
		if r.MCPProxy != (MCPProxyMetrics{}) {
			a.cumulativeMCP = a.cumulativeMCP.Add(r.MCPProxy)
			a.currentMCP = r.MCPProxy
			result.MCPDelta = r.MCPProxy
		}
		return result
	}

	a.currentTokens = r.Tokens
	a.currentModel = r.Model

	key := r.requestKey()
	if key == "" {
		result.TokensDelta = r.Tokens
		result.MCPDelta = r.MCPProxy
		result.TurnDelta = 1
		a.applyUsageDelta("", Tokens{}, r.Model, r.Tokens, result.TurnDelta)
		if result.MCPDelta != (MCPProxyMetrics{}) {
			a.cumulativeMCP = a.cumulativeMCP.Add(result.MCPDelta)
			a.currentMCP = result.MCPDelta
		}
		return result
	}

	state, ok := a.requests[key]
	if !ok {
		state = &assistantRequestState{}
		a.requests[key] = state
		result.TurnDelta = 1
	}

	prevModel := state.model
	prevTokens := state.tokens
	prevMCP := state.mcpProxy
	if r.MCPProxy.ToolUseTurns > 0 {
		state.usesMCP = true
	}
	state.model = r.Model
	state.tokens = r.Tokens
	if state.usesMCP {
		total := state.tokens.Total()
		state.mcpProxy = MCPProxyMetrics{
			Total:         total,
			ToolUseTokens: total,
			ToolUseTurns:  1,
		}
	} else {
		state.mcpProxy = MCPProxyMetrics{}
	}

	result.TokensDelta = state.tokens.Sub(prevTokens)
	a.applyUsageDelta(prevModel, prevTokens, state.model, state.tokens, result.TurnDelta)
	result.MCPDelta = state.mcpProxy.Sub(prevMCP)
	if result.MCPDelta != (MCPProxyMetrics{}) {
		a.cumulativeMCP = a.cumulativeMCP.Add(result.MCPDelta)
		a.currentMCP = state.mcpProxy
	}
	return result
}

// IngestLine parses and ingests a single jsonl line. Malformed lines
// bump ParseErrors and return false. Returns true on usage-bearing lines.
func (a *Aggregator) IngestLine(line []byte) bool {
	rec, err := parseLine(line)
	if err != nil {
		a.parseErrors++
		return false
	}
	return a.Ingest(rec).UsageObserved
}

func (a *Aggregator) applyUsageDelta(prevModel string, prevTokens Tokens, nextModel string, nextTokens Tokens, turnDelta int64) {
	a.cumulative = a.cumulative.Add(nextTokens.Sub(prevTokens))
	a.turns += turnDelta

	switch {
	case prevModel == "" || prevModel == nextModel:
		mt := a.ensureModel(nextModel)
		mt.Totals = mt.Totals.Add(nextTokens.Sub(prevTokens))
		mt.Turns += turnDelta
	default:
		prev := a.ensureModel(prevModel)
		prev.Totals = prev.Totals.Sub(prevTokens)
		prev.Turns--
		if prev.Turns == 0 && prev.Totals == (Tokens{}) {
			delete(a.byModel, prevModel)
		}

		next := a.ensureModel(nextModel)
		next.Totals = next.Totals.Add(nextTokens)
		next.Turns++
	}
}

func (a *Aggregator) ensureModel(model string) *ModelTotals {
	mt, ok := a.byModel[model]
	if ok {
		return mt
	}
	mt = &ModelTotals{Model: model}
	a.byModel[model] = mt
	return mt
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
