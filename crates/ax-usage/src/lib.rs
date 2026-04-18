//! Transcript parsing and usage aggregation.
//!
//! Surface:
//! - Claude transcript line parser.
//! - Aggregator (request-level dedup + cumulative totals).
//! - Codex session parser.
//! - History scan (Claude + Codex) with multi-binding attribution.
//! - Trend query returning the public `WorkspaceTrend` shape.

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
    discover_transcripts, query_history, query_workspace_trends, query_workspace_trends_for,
    scan_workspace_from_project_dir, AgentHistory, Bucket, CurrentSnapshot, HistoryError,
    HistoryQuery, HistoryResponse, WorkspaceBinding, WorkspaceHistory, DEFAULT_BUCKET_SIZE,
    DEFAULT_HISTORY_WINDOW,
};
pub use parse::{parse_line, ParseError, ParsedRecord};
