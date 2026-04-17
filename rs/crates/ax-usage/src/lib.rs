//! Transcript parsing and usage aggregation.
//!
//! Rust port of `internal/usage`. Scope lands in slices:
//! 1. Claude transcript line parser. ✅
//! 2. Aggregator (request-level dedup + cumulative totals). ✅
//! 3. History scan (project-dir discovery + transcriptSeries assembly).
//! 4. Codex session parser.
//! 5. Trend query (bucketing + public snapshot).

#![forbid(unsafe_code)]

mod aggregator;
mod codex;
mod parse;

pub use aggregator::{ingest_line, Aggregator, IngestResult, UsageSnapshot};
pub use codex::{
    parse_codex_line, parsed_record_from_codex, CodexLine, CodexParseError, CODEX_AGENT_NAME,
};
pub use parse::{parse_line, ParseError, ParsedRecord};
