//! Incremental aggregation over transcript records.
//!
//! One [`Aggregator`] corresponds to one transcript file (one session).
//! [`Aggregator::ingest`] takes a [`ParsedRecord`] and folds it into the
//! running totals, deduping assistant requests by their `requestKey()` so
//! revision edits to the same turn update — not double-count — the
//! cumulative numbers.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use ax_proto::usage::{MCPProxyMetrics, ModelTotals, Tokens};

use crate::parse::{parse_line, ParseError, ParsedRecord};

/// Snapshot of the aggregator's state. Kept inside ax-usage for now
/// because the daemon wire representation for live per-workspace
/// usage hasn't been finalised.
#[derive(Debug, Clone, Default)]
pub struct UsageSnapshot {
    pub session_id: String,
    pub transcript_path: String,
    pub session_start: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub cumulative_totals: Tokens,
    pub cumulative_mcp: MCPProxyMetrics,
    pub by_model: Vec<ModelTotals>,
    pub current_context: Tokens,
    pub current_mcp: MCPProxyMetrics,
    pub current_model: String,
    pub turns: i64,
    pub available: bool,
}

/// Effect of a single ingestion — deltas the history bucketing pass uses
/// to attribute this record to its time window.
#[derive(Debug, Clone, Copy, Default)]
pub struct IngestResult {
    pub usage_observed: bool,
    pub tokens_delta: Tokens,
    pub mcp_delta: MCPProxyMetrics,
    pub turn_delta: i64,
}

#[derive(Debug, Default)]
pub struct Aggregator {
    session_id: String,
    session_start: Option<DateTime<Utc>>,
    last_activity: Option<DateTime<Utc>>,

    cumulative: Tokens,
    cumulative_mcp: MCPProxyMetrics,
    by_model: BTreeMap<String, ModelTotals>,
    current_tokens: Tokens,
    current_mcp: MCPProxyMetrics,
    current_model: String,
    turns: i64,

    parse_errors: i64,
    requests: BTreeMap<String, AssistantRequestState>,
}

#[derive(Debug, Default, Clone)]
struct AssistantRequestState {
    model: String,
    tokens: Tokens,
    uses_mcp: bool,
    mcp_proxy: MCPProxyMetrics,
}

impl Aggregator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all accumulated state. Used when the active session file
    /// rotates mid-scan.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    #[must_use]
    pub fn parse_errors(&self) -> i64 {
        self.parse_errors
    }

    #[must_use]
    pub fn turns(&self) -> i64 {
        self.turns
    }

    /// Parse `line` and ingest the result. Malformed lines bump
    /// [`Self::parse_errors`] and return `false`; otherwise returns the
    /// `usage_observed` bit of the effect.
    pub fn ingest_line(&mut self, line: &[u8]) -> bool {
        if let Ok(rec) = parse_line(line) {
            self.ingest(&rec).usage_observed
        } else {
            self.parse_errors += 1;
            false
        }
    }

    /// Fold a parsed record into the running totals.
    pub fn ingest(&mut self, rec: &ParsedRecord) -> IngestResult {
        let mut result = IngestResult {
            usage_observed: rec.has_usage,
            ..IngestResult::default()
        };
        if !rec.session_id.is_empty() {
            self.session_id.clone_from(&rec.session_id);
        }
        if let Some(ts) = rec.timestamp {
            if self.session_start.is_none() {
                self.session_start = Some(ts);
            }
            self.last_activity = Some(ts);
        }
        if !rec.has_usage {
            if rec.mcp_proxy != MCPProxyMetrics::default() {
                self.cumulative_mcp = add_mcp(self.cumulative_mcp, rec.mcp_proxy);
                self.current_mcp = rec.mcp_proxy;
                result.mcp_delta = rec.mcp_proxy;
            }
            return result;
        }

        self.current_tokens = rec.tokens;
        self.current_model.clone_from(&rec.model);

        let key = rec.request_key();
        if key.is_empty() {
            // No dedup key — treat as a fresh turn.
            result.tokens_delta = rec.tokens;
            result.mcp_delta = rec.mcp_proxy;
            result.turn_delta = 1;
            self.apply_usage_delta("", Tokens::default(), &rec.model, rec.tokens, 1);
            if result.mcp_delta != MCPProxyMetrics::default() {
                self.cumulative_mcp = add_mcp(self.cumulative_mcp, result.mcp_delta);
                self.current_mcp = result.mcp_delta;
            }
            return result;
        }

        // First occurrence → turn; subsequent updates to the same key
        // replace the previous snapshot so edits don't double-count.
        let (prev_model, prev_tokens, prev_mcp, new_mcp) = {
            let first_time = !self.requests.contains_key(&key);
            if first_time {
                result.turn_delta = 1;
            }
            let state = self.requests.entry(key).or_default();
            let prev_model = state.model.clone();
            let prev_tokens = state.tokens;
            let prev_mcp = state.mcp_proxy;
            if rec.mcp_proxy.tool_use_turns > 0 {
                state.uses_mcp = true;
            }
            state.model.clone_from(&rec.model);
            state.tokens = rec.tokens;
            state.mcp_proxy = if state.uses_mcp {
                let total = state.tokens.total();
                MCPProxyMetrics {
                    total,
                    tool_use_tokens: total,
                    tool_use_turns: 1,
                    ..MCPProxyMetrics::default()
                }
            } else {
                MCPProxyMetrics::default()
            };
            (prev_model, prev_tokens, prev_mcp, state.mcp_proxy)
        };

        result.tokens_delta = rec.tokens - prev_tokens;
        self.apply_usage_delta(
            &prev_model,
            prev_tokens,
            &rec.model,
            rec.tokens,
            result.turn_delta,
        );
        result.mcp_delta = sub_mcp(new_mcp, prev_mcp);
        if result.mcp_delta != MCPProxyMetrics::default() {
            self.cumulative_mcp = add_mcp(self.cumulative_mcp, result.mcp_delta);
            self.current_mcp = new_mcp;
        }
        result
    }

    /// Materialise the current state into a snapshot. `workspace` and
    /// `transcript_path` are attached unchanged so callers can record
    /// where the data came from.
    #[must_use]
    pub fn snapshot(&self, workspace: &str, transcript_path: &str) -> UsageSnapshot {
        let _ = workspace; // reserved for the eventual public shape
        let mut models: Vec<ModelTotals> = self.by_model.values().cloned().collect();
        models.sort_by(|a, b| a.model.cmp(&b.model));
        UsageSnapshot {
            session_id: self.session_id.clone(),
            transcript_path: transcript_path.to_owned(),
            session_start: self.session_start,
            last_activity: self.last_activity,
            cumulative_totals: self.cumulative,
            cumulative_mcp: self.cumulative_mcp,
            by_model: models,
            current_context: self.current_tokens,
            current_mcp: self.current_mcp,
            current_model: self.current_model.clone(),
            turns: self.turns,
            available: self.turns > 0 || !self.session_id.is_empty(),
        }
    }

    // ---------- internals ----------

    fn apply_usage_delta(
        &mut self,
        prev_model: &str,
        prev_tokens: Tokens,
        next_model: &str,
        next_tokens: Tokens,
        turn_delta: i64,
    ) {
        self.cumulative = self.cumulative + (next_tokens - prev_tokens);
        self.turns += turn_delta;
        if prev_model.is_empty() || prev_model == next_model {
            let mt = self.ensure_model(next_model);
            mt.totals = mt.totals + (next_tokens - prev_tokens);
            mt.turns += turn_delta;
            return;
        }
        // Model change: remove prev-model share, add next-model share.
        let prev = self.ensure_model(prev_model);
        prev.totals = prev.totals - prev_tokens;
        prev.turns -= 1;
        let should_drop = prev.turns == 0 && prev.totals == Tokens::default();
        if should_drop {
            self.by_model.remove(prev_model);
        }
        let next = self.ensure_model(next_model);
        next.totals = next.totals + next_tokens;
        next.turns += 1;
    }

    fn ensure_model(&mut self, model: &str) -> &mut ModelTotals {
        self.by_model
            .entry(model.to_owned())
            .or_insert_with(|| ModelTotals {
                model: model.to_owned(),
                turns: 0,
                totals: Tokens::default(),
            })
    }
}

fn add_mcp(a: MCPProxyMetrics, b: MCPProxyMetrics) -> MCPProxyMetrics {
    MCPProxyMetrics {
        total: a.total + b.total,
        prompt_tokens: a.prompt_tokens + b.prompt_tokens,
        prompt_signals: a.prompt_signals + b.prompt_signals,
        tool_use_tokens: a.tool_use_tokens + b.tool_use_tokens,
        tool_use_turns: a.tool_use_turns + b.tool_use_turns,
    }
}

fn sub_mcp(a: MCPProxyMetrics, b: MCPProxyMetrics) -> MCPProxyMetrics {
    MCPProxyMetrics {
        total: a.total - b.total,
        prompt_tokens: a.prompt_tokens - b.prompt_tokens,
        prompt_signals: a.prompt_signals - b.prompt_signals,
        tool_use_tokens: a.tool_use_tokens - b.tool_use_tokens,
        tool_use_turns: a.tool_use_turns - b.tool_use_turns,
    }
}

/// Compatibility shim so callers can feed an aggregator from raw JSONL
/// lines without pulling in [`parse_line`] directly.
pub fn ingest_line<'a>(
    agg: &'a mut Aggregator,
    line: &[u8],
) -> Result<&'a mut Aggregator, ParseError> {
    let rec = parse_line(line)?;
    agg.ingest(&rec);
    Ok(agg)
}
