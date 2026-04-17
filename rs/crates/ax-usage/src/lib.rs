//! Transcript parsing and usage aggregation.
//!
//! Rust port of `internal/usage`. Scope lands in slices:
//! 1. Claude transcript line parser. ✅
//! 2. Aggregator (request-level dedup + cumulative totals). ✅
//! 3. Codex session parser. ✅
//! 4. History scan (single-binding, Claude-only). ✅
//! 5. Multi-binding attribution + Codex integration.
//! 6. Trend query (public `WorkspaceTrend` shape).

#![forbid(unsafe_code)]

mod aggregator;
mod codex;
mod history;
mod parse;

pub use aggregator::{ingest_line, Aggregator, IngestResult, UsageSnapshot};
pub use codex::{
    parse_codex_line, parsed_record_from_codex, CodexLine, CodexParseError, CODEX_AGENT_NAME,
};
pub use history::{
    discover_transcripts, query_history, query_workspace_trends, scan_workspace_from_project_dir,
    AgentHistory, Bucket, CurrentSnapshot, HistoryError, HistoryQuery, HistoryResponse,
    WorkspaceBinding, WorkspaceHistory, DEFAULT_BUCKET_SIZE, DEFAULT_HISTORY_WINDOW,
};
pub use parse::{parse_line, ParseError, ParsedRecord};
