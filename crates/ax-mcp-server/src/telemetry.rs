//! Append-only tool-call telemetry for the MCP server.
//!
//! Each invocation of a tool through the MCP protocol writes one
//! JSONL record into `<state_dir>/telemetry/tool_calls.jsonl`. The
//! goal is to produce real usage data we can query after-the-fact to
//! shape role filtering (Phase 4) instead of guessing. Writes are
//! best-effort — telemetry failures MUST NOT bubble up into tool
//! results.
//!
//! Privacy: we record tool names + outcomes + timing, not the raw
//! parameters, since payloads may contain user-sensitive content.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelemetryEvent {
    pub ts: DateTime<Utc>,
    pub workspace: String,
    pub tool: String,
    pub ok: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub err_kind: String,
}

/// Best-effort sink that appends one JSON object per record. Keep it
/// cheap: each call opens+writes+closes the file so crashes can't
/// lose buffered lines. Volume is bounded by tool-call rate (<< 1/s
/// in practice), so the open cost is irrelevant.
#[derive(Debug, Clone)]
pub struct TelemetrySink {
    path: PathBuf,
}

impl TelemetrySink {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event. Ignored-and-logged on failure so tool calls
    /// never fail because the telemetry file can't be opened.
    pub fn record(&self, event: &TelemetryEvent) {
        if let Err(e) = self.record_inner(event) {
            tracing::warn!(telemetry_path = %self.path.display(), error = %e, "telemetry write failed");
        }
    }

    fn record_inner(&self, event: &TelemetryEvent) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut line = serde_json::to_vec(event)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push(b'\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(&line)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn event(tool: &str, ok: bool) -> TelemetryEvent {
        TelemetryEvent {
            ts: chrono::Utc::now(),
            workspace: "alpha".into(),
            tool: tool.into(),
            ok,
            duration_ms: 12,
            err_kind: if ok { String::new() } else { "boom".into() },
        }
    }

    #[test]
    fn sink_appends_one_jsonl_record_per_call() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("tool_calls.jsonl");
        let sink = TelemetrySink::new(&path);
        sink.record(&event("list_tasks", true));
        sink.record(&event("send_message", false));
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: TelemetryEvent = serde_json::from_str(lines[0]).unwrap();
        let second: TelemetryEvent = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first.tool, "list_tasks");
        assert!(first.ok);
        assert_eq!(second.tool, "send_message");
        assert!(!second.ok);
        assert_eq!(second.err_kind, "boom");
    }

    #[test]
    fn sink_swallows_errors_silently_when_path_is_unusable() {
        // A path whose parent is a regular file can't become a dir —
        // record() must not panic, only log.
        let tmp = TempDir::new().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "file not dir").unwrap();
        let sink = TelemetrySink::new(blocker.join("tool_calls.jsonl"));
        sink.record(&event("list_tasks", true)); // must not panic
    }
}
