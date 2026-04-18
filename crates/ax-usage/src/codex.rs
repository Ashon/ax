//! Codex session rollout JSONL parser.
//!
//! Codex emits two line shapes we care about:
//!   - `type=session_meta` once at the top of the file, carrying the
//!     session id, cwd, and timestamp.
//!   - `type=event_msg` repeatedly; we only attribute `token_count`
//!     events whose `info` is populated. Codex also emits `token_count`
//!     with `info: null` before any model call (rate-limit pulse) —
//!     those are skipped.
//!
//! A populated `token_count` event carries both `total_token_usage`
//! (cumulative since session start) and `last_token_usage` (per-turn
//! delta); we treat the latter as the turn's tokens because the
//! aggregator expects per-turn deltas, not cumulative snapshots.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use ax_proto::usage::{MCPProxyMetrics, Tokens};

use crate::aggregator::Aggregator;
use crate::history::{Bucket, CurrentSnapshot, HistoryError, HistoryQuery, TranscriptSeries};
use crate::parse::ParsedRecord;

/// Synthetic agent label attached to every Codex-derived parsed record.
pub const CODEX_AGENT_NAME: &str = "codex";

/// Classification of a single JSONL line from a Codex rollout.
#[derive(Debug, Clone)]
pub enum CodexLine {
    /// First line of the file: session identity.
    SessionMeta {
        session_id: String,
        cwd: String,
        timestamp: Option<DateTime<Utc>>,
    },
    /// A populated `token_count` event (rate-limit-only pulses are skipped).
    TokenCount {
        timestamp: Option<DateTime<Utc>>,
        /// Cumulative usage across the session at this moment. Not used
        /// by the aggregator but exposed for callers that want to
        /// display context-window headroom.
        cumulative: Tokens,
        /// Per-turn delta that the aggregator should ingest.
        delta: Tokens,
    },
    /// Any other line we don't attribute (`session_footer`, tool events,
    /// text deltas, rate-limit-only pulses).
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum CodexParseError {
    #[error("decode codex line: {0}")]
    Json(#[from] serde_json::Error),
}

/// Parse one JSONL line from a Codex session file.
pub fn parse_codex_line(data: &[u8]) -> Result<CodexLine, CodexParseError> {
    let raw: RawCodexRecord = serde_json::from_slice(data)?;
    Ok(classify(raw))
}

/// Build the synthetic [`ParsedRecord`] that the [`crate::Aggregator`]
/// ingests for a Codex `token_count` event. `session_id` / `cwd` come
/// from an earlier `SessionMeta` line on the same file.
#[must_use]
pub fn parsed_record_from_codex(
    session_id: &str,
    cwd: &str,
    timestamp: Option<DateTime<Utc>>,
    delta: Tokens,
) -> ParsedRecord {
    ParsedRecord {
        session_id: session_id.to_owned(),
        cwd: cwd.to_owned(),
        timestamp,
        model: CODEX_AGENT_NAME.to_owned(),
        tokens: delta,
        has_usage: true,
        ..ParsedRecord::default()
    }
}

// ---------- serde view ----------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawCodexRecord {
    timestamp: Option<DateTime<Utc>>,
    #[serde(rename = "type")]
    line_type: String,
    payload: serde_json::Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawSessionMetaPayload {
    id: String,
    cwd: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawEventPayload {
    #[serde(rename = "type")]
    event_type: String,
    info: Option<RawTokenInfo>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawTokenInfo {
    total_token_usage: RawCodexUsage,
    last_token_usage: RawCodexUsage,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_field_names)] // JSON keys verbatim from codex
struct RawCodexUsage {
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    #[allow(dead_code)]
    reasoning_output_tokens: i64,
    #[allow(dead_code)]
    total_tokens: i64,
}

impl RawCodexUsage {
    fn to_tokens(&self) -> Tokens {
        let input = (self.input_tokens - self.cached_input_tokens).max(0);
        Tokens {
            input,
            output: self.output_tokens,
            cache_read: self.cached_input_tokens,
            cache_creation: 0,
        }
    }
}

fn classify(raw: RawCodexRecord) -> CodexLine {
    match raw.line_type.as_str() {
        "session_meta" => {
            let meta: RawSessionMetaPayload =
                serde_json::from_value(raw.payload).unwrap_or_default();
            CodexLine::SessionMeta {
                session_id: meta.id,
                cwd: meta.cwd,
                timestamp: raw.timestamp,
            }
        }
        "event_msg" => {
            let ev: RawEventPayload = serde_json::from_value(raw.payload).unwrap_or_default();
            if ev.event_type != "token_count" {
                return CodexLine::Other;
            }
            let Some(info) = ev.info else {
                return CodexLine::Other;
            };
            let delta = info.last_token_usage.to_tokens();
            if delta == Tokens::default() {
                return CodexLine::Other;
            }
            CodexLine::TokenCount {
                timestamp: raw.timestamp,
                cumulative: info.total_token_usage.to_tokens(),
                delta,
            }
        }
        _ => CodexLine::Other,
    }
}

// ---------- session discovery + scan ----------

/// Outcome of a Codex scan for one workspace binding. The status flags
/// let the caller pick the right `unavailable_reason` when both Claude
/// and Codex come up empty.
#[derive(Debug, Default)]
pub(crate) struct CodexScanResult {
    pub home_exists: bool,
    pub sessions_found: bool,
    pub series: Vec<TranscriptSeries>,
}

/// Walk `sessions_dir` for every `rollout-*.jsonl` whose modification
/// time falls at or after `since`. Returns an empty list when the
/// directory doesn't exist. Codex organises files under
/// `YYYY/MM/DD/rollout-*.jsonl`; we walk the full tree rather than
/// predicting dates so cross-midnight sessions get picked up.
pub(crate) fn discover_codex_sessions(
    sessions_dir: &Path,
    since: DateTime<Utc>,
) -> Result<Vec<PathBuf>, HistoryError> {
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    walk_sessions(sessions_dir, since, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_sessions(
    dir: &Path,
    since: DateTime<Utc>,
    out: &mut Vec<PathBuf>,
) -> Result<(), HistoryError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(HistoryError::Read {
                path: dir.display().to_string(),
                source: e,
            })
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| HistoryError::Read {
            path: dir.display().to_string(),
            source: e,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| HistoryError::Read {
            path: path.display().to_string(),
            source: e,
        })?;
        if file_type.is_dir() {
            walk_sessions(&path, since, out)?;
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if since.timestamp() > 0 {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let modified: DateTime<Utc> = modified.into();
            if modified < since {
                continue;
            }
        }
        out.push(path);
    }
    Ok(())
}

/// Scan every Codex session under `home_dir` for `workspace`. Missing
/// `home_dir` yields an empty result (homes only exist once codex has
/// run against that workspace at least once).
pub(crate) fn scan_codex_for_binding(
    workspace: &str,
    home_dir: &Path,
    query: &HistoryQuery,
) -> Result<CodexScanResult, HistoryError> {
    let _ = workspace; // reserved for future multi-agent labeling
    let mut res = CodexScanResult::default();
    if !home_dir.exists() {
        return Ok(res);
    }
    res.home_exists = true;
    let sessions_dir = home_dir.join("sessions");
    let paths = discover_codex_sessions(&sessions_dir, query.since)?;
    if paths.is_empty() {
        return Ok(res);
    }
    res.sessions_found = true;
    for path in paths {
        if let Some(series) = scan_codex_transcript(&path, query)? {
            res.series.push(series);
        }
    }
    Ok(res)
}

fn scan_codex_transcript(
    path: &Path,
    query: &HistoryQuery,
) -> Result<Option<TranscriptSeries>, HistoryError> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).map_err(|e| HistoryError::Read {
        path: path.display().to_string(),
        source: e,
    })?;
    let reader = std::io::BufReader::new(file);

    let mut agg = Aggregator::new();
    let mut buckets: BTreeMap<DateTime<Utc>, Bucket> = BTreeMap::new();
    let mut series = TranscriptSeries {
        agent: CODEX_AGENT_NAME.to_owned(),
        transcript: path.to_path_buf(),
        ..TranscriptSeries::default()
    };

    for line in reader.lines() {
        let line = line.map_err(|e| HistoryError::Read {
            path: path.display().to_string(),
            source: e,
        })?;
        let Ok(parsed) = parse_codex_line(line.as_bytes()) else {
            continue;
        };
        match parsed {
            CodexLine::SessionMeta {
                session_id, cwd, ..
            } => {
                if series.session_id.is_empty() && !session_id.is_empty() {
                    series.session_id = session_id;
                }
                if series.cwd.is_empty() && !cwd.is_empty() {
                    series.cwd = cwd;
                }
            }
            CodexLine::TokenCount {
                timestamp, delta, ..
            } => {
                let rec =
                    parsed_record_from_codex(&series.session_id, &series.cwd, timestamp, delta);
                let effect = agg.ingest(&rec);
                let Some(ts) = timestamp else { continue };
                if ts < query.since || ts >= query.until {
                    continue;
                }
                if effect.turn_delta == 0 && effect.tokens_delta == Tokens::default() {
                    continue;
                }
                let start = truncate_to_bucket(ts, query.bucket_size);
                let bucket_duration = chrono::Duration::from_std(query.bucket_size)
                    .unwrap_or(chrono::Duration::zero());
                let b = buckets.entry(start).or_insert_with(|| Bucket {
                    start,
                    end: start + bucket_duration,
                    ..Bucket::default()
                });
                if effect.tokens_delta != Tokens::default() {
                    b.tokens = b.tokens + effect.tokens_delta;
                    b.total += effect.tokens_delta.total();
                }
                if effect.turn_delta != 0 {
                    b.turns += effect.turn_delta;
                }
            }
            CodexLine::Other => {}
        }
    }

    let snap = agg.snapshot("", &series.transcript.display().to_string());
    if !snap.available && series.session_id.is_empty() {
        return Ok(None);
    }
    series.current = CurrentSnapshot {
        last_activity: snap.last_activity,
        current_context: snap.current_context,
        current_total: snap.current_context.total(),
        current_mcp_proxy: snap.current_mcp,
        current_model: snap.current_model,
        cumulative_totals: snap.cumulative_totals,
        cumulative_total: snap.cumulative_totals.total(),
        cumulative_mcp_proxy: snap.cumulative_mcp,
        turns: snap.turns,
    };
    series.buckets = buckets.into_values().collect();
    Ok(Some(series))
}

fn truncate_to_bucket(ts: DateTime<Utc>, bucket: std::time::Duration) -> DateTime<Utc> {
    let secs = bucket.as_secs() as i64;
    if secs == 0 {
        return ts;
    }
    let epoch = ts.timestamp();
    let truncated = epoch - epoch.rem_euclid(secs);
    DateTime::<Utc>::from_timestamp(truncated, 0).unwrap_or(ts)
}

#[allow(dead_code)]
fn _use_mcp(_: MCPProxyMetrics) {}
