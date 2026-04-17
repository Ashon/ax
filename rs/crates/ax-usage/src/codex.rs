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
//! delta); we treat the latter as the turn's tokens because the Go
//! aggregator semantics expect per-turn deltas, not cumulative snapshots.
//!
//! Mirrors `internal/usage/codex.go`.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use ax_proto::usage::Tokens;

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
