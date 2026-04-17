package usage

import (
	"bufio"
	"encoding/json"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"time"

	"github.com/ashon/ax/internal/agent"
)

// codexAgentName is the synthetic agent label attached to every Codex-derived
// transcriptSeries so per-workspace rollups can still report "codex" activity
// even without a sub-agent identifier like Claude's agentId.
const codexAgentName = "codex"

// codexRecord is the on-disk shape for one JSONL line in a Codex session
// rollout file. Codex emits both session metadata (`type=session_meta`) and
// per-event messages (`type=event_msg`); we only care about the meta header
// and `token_count` events within that stream.
type codexRecord struct {
	Timestamp time.Time       `json:"timestamp"`
	Type      string          `json:"type"`
	Payload   json.RawMessage `json:"payload"`
}

type codexSessionMetaPayload struct {
	ID        string    `json:"id"`
	Timestamp time.Time `json:"timestamp"`
	Cwd       string    `json:"cwd"`
}

type codexEventPayload struct {
	Type string          `json:"type"`
	Info *codexTokenInfo `json:"info"`
}

type codexTokenInfo struct {
	TotalTokenUsage codexTokenUsage `json:"total_token_usage"`
	LastTokenUsage  codexTokenUsage `json:"last_token_usage"`
	// ModelContextWindow is reported but we do not currently persist it.
}

// codexTokenUsage mirrors the `total_token_usage` / `last_token_usage` shape
// Codex writes. Note that `input_tokens` is INCLUSIVE of `cached_input_tokens`
// (confirmed via total_tokens == input_tokens + output_tokens); we subtract
// `cached_input_tokens` before mapping into the runtime-neutral `Tokens`
// struct so cumulative totals stay consistent across runtimes.
type codexTokenUsage struct {
	InputTokens         int64 `json:"input_tokens"`
	CachedInputTokens   int64 `json:"cached_input_tokens"`
	OutputTokens        int64 `json:"output_tokens"`
	ReasoningOutputTok  int64 `json:"reasoning_output_tokens"`
	TotalTokens         int64 `json:"total_tokens"`
}

func (u codexTokenUsage) toTokens() Tokens {
	input := u.InputTokens - u.CachedInputTokens
	if input < 0 {
		input = 0
	}
	return Tokens{
		Input:     input,
		Output:    u.OutputTokens,
		CacheRead: u.CachedInputTokens,
		// Codex has no distinct "cache creation" concept; leave at zero.
	}
}

// discoverCodexSessions returns every rollout JSONL under a workspace's
// Codex home whose mtime falls inside [since, until]. Codex organises files
// as $CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl; we walk the full tree
// rather than trying to predict dates, since session files can be written
// across day boundaries.
func discoverCodexSessions(sessionsDir string, since, until time.Time) ([]string, error) {
	paths := make([]string, 0, 16)
	err := filepath.WalkDir(sessionsDir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			if errors.Is(err, os.ErrNotExist) {
				return filepath.SkipAll
			}
			return err
		}
		if d.IsDir() {
			return nil
		}
		if filepath.Ext(path) != ".jsonl" {
			return nil
		}
		info, statErr := d.Info()
		if statErr != nil {
			return nil
		}
		if !since.IsZero() && info.ModTime().Before(since) {
			return nil
		}
		paths = append(paths, path)
		return nil
	})
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, nil
		}
		return nil, err
	}
	sort.Strings(paths)
	return paths, nil
}

// scanCodexTranscript reads a single Codex session file and produces a
// transcriptSeries with per-turn buckets. Turns here correspond to
// token_count events whose `info.last_token_usage` is populated; Codex also
// emits a no-op token_count before any model call that only carries a
// rate-limit snapshot, which we skip.
func scanCodexTranscript(path string, q HistoryQuery) (*transcriptSeries, error) {
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
		agent:      codexAgentName,
		transcript: path,
	}

	for scanner.Scan() {
		var rec codexRecord
		if err := json.Unmarshal(scanner.Bytes(), &rec); err != nil {
			continue
		}
		switch rec.Type {
		case "session_meta":
			var meta codexSessionMetaPayload
			if err := json.Unmarshal(rec.Payload, &meta); err != nil {
				continue
			}
			if series.sessionID == "" {
				series.sessionID = meta.ID
			}
			if series.cwd == "" && meta.Cwd != "" {
				series.cwd = cleanPath(meta.Cwd)
			}
		case "event_msg":
			var ev codexEventPayload
			if err := json.Unmarshal(rec.Payload, &ev); err != nil {
				continue
			}
			if ev.Type != "token_count" || ev.Info == nil {
				continue
			}
			delta := ev.Info.LastTokenUsage.toTokens()
			if delta == (Tokens{}) {
				continue
			}
			parsed := parsedRecord{
				SessionID: series.sessionID,
				Cwd:       series.cwd,
				Timestamp: rec.Timestamp,
				Model:     codexAgentName,
				Tokens:    delta,
				HasUsage:  true,
			}
			effect := agg.Ingest(parsed)
			if rec.Timestamp.IsZero() || rec.Timestamp.Before(q.Since) || !rec.Timestamp.Before(q.Until) {
				continue
			}
			if effect.TurnDelta == 0 && effect.TokensDelta == (Tokens{}) {
				continue
			}
			start := rec.Timestamp.UTC().Truncate(q.BucketSize)
			b := buckets[start]
			if b == nil {
				b = &Bucket{Start: start, End: start.Add(q.BucketSize)}
				buckets[start] = b
			}
			if effect.TokensDelta != (Tokens{}) {
				b.Tokens = b.Tokens.Add(effect.TokensDelta)
				b.Total += effect.TokensDelta.Total()
			}
			if effect.TurnDelta != 0 {
				b.Turns += effect.TurnDelta
			}
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}
	snap := agg.Snapshot("", path)
	if !snap.Available && series.sessionID == "" {
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

// codexScanResult captures the per-workspace scan outcome for Codex. Unlike
// the Claude code path, each session file belongs to a known workspace
// (Codex runs with a workspace-specific CODEX_HOME), so attribution is
// deterministic and no hint matching is required.
type codexScanResult struct {
	homeExists    bool
	sessionsFound bool
	series        []*transcriptSeries
}

// scanCodexForBinding returns every Codex transcript series attributable to
// one ax workspace. Missing CODEX_HOME directories yield an empty result
// rather than an error so Claude-only workspaces still work.
func scanCodexForBinding(binding WorkspaceBinding, q HistoryQuery) (codexScanResult, error) {
	var res codexScanResult
	if binding.Name == "" || binding.Dir == "" {
		return res, nil
	}
	home, err := agent.CodexHomePath(binding.Name, binding.Dir)
	if err != nil {
		return res, err
	}
	if _, err := os.Stat(home); err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return res, nil
		}
		return res, err
	}
	res.homeExists = true

	sessionsDir := filepath.Join(home, "sessions")
	paths, err := discoverCodexSessions(sessionsDir, q.Since, q.Until)
	if err != nil {
		return res, err
	}
	if len(paths) == 0 {
		return res, nil
	}
	res.sessionsFound = true
	for _, path := range paths {
		s, err := scanCodexTranscript(path, q)
		if err != nil {
			return res, err
		}
		if s != nil {
			res.series = append(res.series, s)
		}
	}
	return res, nil
}
