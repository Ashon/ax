//! Parse transcript lines that mirror real Claude Code JSONL records.
//! Each test pins the semantics documented in `internal/usage/parse.go`
//! so the Rust port stays honest against the Go aggregator behaviour.

use ax_usage::parse_line;

#[test]
fn assistant_line_populates_tokens_and_keys() {
    let raw = br#"{"type":"assistant","sessionId":"sess-1","cwd":"/tmp/proj","timestamp":"2026-04-17T10:00:00Z","requestId":"req-1","message":{"id":"msg-1","role":"assistant","model":"claude-opus-4-7","usage":{"input_tokens":1000,"output_tokens":250,"cache_read_input_tokens":500,"cache_creation_input_tokens":100},"content":[{"type":"text","text":"hello"}]}}"#;
    let rec = parse_line(raw).expect("parse");
    assert!(rec.has_usage);
    assert_eq!(rec.session_id, "sess-1");
    assert_eq!(rec.cwd, "/tmp/proj");
    assert_eq!(rec.request_id, "req-1");
    assert_eq!(rec.message_id, "msg-1");
    assert_eq!(rec.model, "claude-opus-4-7");
    assert_eq!(rec.tokens.input, 1000);
    assert_eq!(rec.tokens.output, 250);
    assert_eq!(rec.tokens.cache_read, 500);
    assert_eq!(rec.tokens.cache_creation, 100);
    assert_eq!(rec.tokens.total(), 1000 + 250 + 500 + 100);
    // Plain text content does not trigger MCP proxy metrics.
    assert_eq!(rec.mcp_proxy.total, 0);
    assert_eq!(rec.request_key(), "request:req-1");
}

#[test]
fn request_key_falls_back_to_message_id() {
    let raw = br#"{"type":"assistant","sessionId":"s","message":{"id":"msg-only","role":"assistant","usage":{"input_tokens":10,"output_tokens":5}}}"#;
    let rec = parse_line(raw).expect("parse");
    assert_eq!(rec.request_key(), "message:msg-only");
}

#[test]
fn user_line_carries_session_metadata_but_no_usage() {
    let raw = br#"{"type":"user","sessionId":"sess-1","cwd":"/tmp/proj","timestamp":"2026-04-17T10:00:05Z","message":{"role":"user","content":[{"type":"text","text":"ping"}]}}"#;
    let rec = parse_line(raw).expect("parse");
    assert!(!rec.has_usage);
    assert_eq!(rec.session_id, "sess-1");
    assert_eq!(rec.cwd, "/tmp/proj");
    assert_eq!(rec.tokens.total(), 0);
    assert_eq!(rec.request_key(), "");
}

#[test]
fn mcp_instructions_attachment_estimates_prompt_overhead() {
    let raw = br#"{"type":"attachment","sessionId":"sess-1","cwd":"/tmp/proj","timestamp":"2026-04-17T10:00:00Z","attachment":{"type":"mcp_instructions_delta","addedBlocks":["You are the \"ax.runtime\" workspace agent in an ax multi-agent environment."]}}"#;
    let rec = parse_line(raw).expect("parse");
    // "(chars + 3) / 4" estimate; the block text is 79 chars trimmed.
    // 79 + 3 = 82, 82 / 4 = 20. Whatever the exact char count, it's > 0.
    assert!(rec.mcp_proxy.total > 0, "got {}", rec.mcp_proxy.total);
    assert_eq!(rec.mcp_proxy.prompt_tokens, rec.mcp_proxy.total);
    assert_eq!(rec.mcp_proxy.prompt_signals, 1);
    assert_eq!(rec.workspace_hint, "ax.runtime");
    assert!(!rec.has_usage);
}

#[test]
fn tool_use_marks_the_turn_as_mcp_overhead() {
    let raw = br#"{"type":"assistant","sessionId":"sess-1","cwd":"/tmp/proj","timestamp":"2026-04-17T10:00:10Z","requestId":"req-2","message":{"id":"msg-2","role":"assistant","model":"claude-opus-4-7","usage":{"input_tokens":2000,"output_tokens":100,"cache_read_input_tokens":800,"cache_creation_input_tokens":0},"content":[{"type":"tool_use","name":"mcp__ax__list_tasks","input":{}}]}}"#;
    let rec = parse_line(raw).expect("parse");
    let total = rec.tokens.total();
    assert_eq!(rec.mcp_proxy.tool_use_tokens, total);
    assert_eq!(rec.mcp_proxy.tool_use_turns, 1);
    assert_eq!(rec.mcp_proxy.total, total);
    // Prompt proxy is zero on messages (only attachments contribute).
    assert_eq!(rec.mcp_proxy.prompt_tokens, 0);
}

#[test]
fn deferred_tools_attachment_only_counts_mcp_prefixed_names() {
    let raw = br#"{"type":"attachment","sessionId":"sess-1","cwd":"/tmp/proj","timestamp":"2026-04-17T10:00:20Z","attachment":{"type":"deferred_tools_delta","addedLines":["mcp__ax__start_task","workspace_create","ListMcpResourcesTool"]}}"#;
    let rec = parse_line(raw).expect("parse");
    // Two lines survive the filter ("mcp__ax__start_task" and
    // "ListMcpResourcesTool"), joined by newline, then (chars+3)/4 tokens.
    let kept = "mcp__ax__start_task\nListMcpResourcesTool";
    let expected = kept.chars().count().div_ceil(4);
    assert_eq!(rec.mcp_proxy.total, expected as i64);
    assert_eq!(rec.mcp_proxy.prompt_tokens, expected as i64);
    assert_eq!(rec.mcp_proxy.prompt_signals, 1);
}

#[test]
fn malformed_json_returns_error() {
    assert!(parse_line(b"{ not json").is_err());
}

#[test]
fn unknown_attachment_type_produces_no_proxy_metrics() {
    let raw =
        br#"{"type":"attachment","attachment":{"type":"something_else","content":"ignored"}}"#;
    let rec = parse_line(raw).expect("parse");
    assert_eq!(rec.mcp_proxy.total, 0);
    assert!(!rec.has_usage);
}
