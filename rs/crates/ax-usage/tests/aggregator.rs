//! Aggregator behaviour pinned against Claude transcript sequences.
//!
//! Key behaviours exercised (all mirror `internal/usage/aggregator.go`):
//!   - Each unique request key counts as exactly one turn.
//!   - Updates to the same request key replace (not add to) the turn's
//!     tokens, so editor-style edits don't double-count usage.
//!   - Records lacking a request key fall through to per-line accounting.
//!   - MCP-proxy totals from attachments and tool-use turns land on the
//!     cumulative rollup.
//!   - Parse errors bump the counter without corrupting aggregate state.

use ax_usage::{ingest_line, Aggregator};

fn line_assistant(req: &str, msg: &str, input: i64, output: i64) -> String {
    format!(
        r#"{{"type":"assistant","sessionId":"sess-1","cwd":"/tmp/proj","timestamp":"2026-04-17T10:00:00Z","requestId":"{req}","message":{{"id":"{msg}","role":"assistant","model":"claude-opus-4-7","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"content":[{{"type":"text","text":"hi"}}]}}}}"#,
    )
}

fn line_tool_use(req: &str, msg: &str, input: i64, output: i64) -> String {
    format!(
        r#"{{"type":"assistant","sessionId":"sess-1","cwd":"/tmp/proj","timestamp":"2026-04-17T10:00:00Z","requestId":"{req}","message":{{"id":"{msg}","role":"assistant","model":"claude-opus-4-7","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"content":[{{"type":"tool_use","name":"mcp__ax__list_tasks","input":{{}}}}]}}}}"#,
    )
}

#[test]
fn two_distinct_requests_give_two_turns_and_cumulative_sum() {
    let mut agg = Aggregator::new();
    ingest_line(
        &mut agg,
        line_assistant("req-1", "msg-1", 1000, 200).as_bytes(),
    )
    .unwrap();
    ingest_line(
        &mut agg,
        line_assistant("req-2", "msg-2", 500, 100).as_bytes(),
    )
    .unwrap();

    let snap = agg.snapshot("ax.cli", "/tmp/transcript.jsonl");
    assert_eq!(snap.turns, 2);
    assert_eq!(snap.cumulative_totals.input, 1500);
    assert_eq!(snap.cumulative_totals.output, 300);
    assert!(snap.available);
    assert_eq!(snap.session_id, "sess-1");
}

#[test]
fn same_request_id_replaces_instead_of_adding() {
    let mut agg = Aggregator::new();
    // First emission of req-1 with small totals.
    ingest_line(
        &mut agg,
        line_assistant("req-1", "msg-1", 100, 20).as_bytes(),
    )
    .unwrap();
    // Same req-1, updated totals — Claude does this when the turn grows.
    ingest_line(
        &mut agg,
        line_assistant("req-1", "msg-1", 1000, 200).as_bytes(),
    )
    .unwrap();

    let snap = agg.snapshot("ax.cli", "/tmp/transcript.jsonl");
    assert_eq!(snap.turns, 1, "still one turn");
    assert_eq!(snap.cumulative_totals.input, 1000);
    assert_eq!(snap.cumulative_totals.output, 200);
}

#[test]
fn records_without_request_key_still_count_as_turns() {
    // No `requestId`, no `message.id` → the parser yields an empty
    // request_key and the aggregator treats the record as a fresh turn.
    let raw = r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-04-17T10:00:00Z","message":{"role":"assistant","model":"m","usage":{"input_tokens":50,"output_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#;
    let mut agg = Aggregator::new();
    ingest_line(&mut agg, raw.as_bytes()).unwrap();
    ingest_line(&mut agg, raw.as_bytes()).unwrap();

    let snap = agg.snapshot("ax.cli", "/tmp/transcript.jsonl");
    assert_eq!(snap.turns, 2);
    assert_eq!(snap.cumulative_totals.input, 100);
}

#[test]
fn tool_use_turn_reports_mcp_proxy_metrics() {
    let mut agg = Aggregator::new();
    ingest_line(
        &mut agg,
        line_tool_use("req-1", "msg-1", 1000, 200).as_bytes(),
    )
    .unwrap();
    let snap = agg.snapshot("ax.cli", "/tmp/transcript.jsonl");
    assert_eq!(snap.cumulative_mcp.tool_use_turns, 1);
    let total = snap.cumulative_totals.total();
    assert_eq!(snap.cumulative_mcp.tool_use_tokens, total);
    assert_eq!(snap.cumulative_mcp.total, total);
}

#[test]
fn ingest_line_bumps_parse_errors_on_bad_json_without_polluting_state() {
    let mut agg = Aggregator::new();
    assert!(!agg.ingest_line(b"{ malformed"));
    assert_eq!(agg.parse_errors(), 1);
    ingest_line(&mut agg, line_assistant("req-1", "msg-1", 10, 5).as_bytes()).unwrap();
    assert_eq!(agg.turns(), 1);
    assert_eq!(agg.parse_errors(), 1);
}

#[test]
fn reset_clears_all_accumulated_state() {
    let mut agg = Aggregator::new();
    ingest_line(&mut agg, line_assistant("req-1", "msg-1", 10, 5).as_bytes()).unwrap();
    agg.reset();
    assert_eq!(agg.turns(), 0);
    let snap = agg.snapshot("ax.cli", "");
    assert_eq!(snap.cumulative_totals.total(), 0);
    assert!(!snap.available);
}

#[test]
fn snapshot_by_model_accumulates_per_model_turns_and_totals() {
    let other_model = r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-04-17T10:00:00Z","requestId":"req-2","message":{"id":"msg-2","role":"assistant","model":"claude-sonnet-4-6","usage":{"input_tokens":200,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#;
    let mut agg = Aggregator::new();
    ingest_line(
        &mut agg,
        line_assistant("req-1", "msg-1", 1000, 200).as_bytes(),
    )
    .unwrap();
    ingest_line(&mut agg, other_model.as_bytes()).unwrap();

    let snap = agg.snapshot("ax.cli", "/tmp/transcript.jsonl");
    let opus = snap
        .by_model
        .iter()
        .find(|m| m.model == "claude-opus-4-7")
        .expect("opus entry");
    let sonnet = snap
        .by_model
        .iter()
        .find(|m| m.model == "claude-sonnet-4-6")
        .expect("sonnet entry");
    assert_eq!(opus.turns, 1);
    assert_eq!(opus.totals.input, 1000);
    assert_eq!(sonnet.turns, 1);
    assert_eq!(sonnet.totals.input, 200);
}
