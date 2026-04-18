//! Codex rollout line parser, including integration with the aggregator.

use ax_usage::{
    parse_codex_line, parsed_record_from_codex, Aggregator, CodexLine, CODEX_AGENT_NAME,
};

const META_LINE: &str = r#"{"timestamp":"2026-04-14T15:25:54.762Z","type":"session_meta","payload":{"id":"sess-codex-1","timestamp":"2026-04-14T15:21:09.246Z","cwd":"/tmp/codex-workspace"}}"#;

const NULL_INFO_LINE: &str = r#"{"timestamp":"2026-04-14T15:25:55.005Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex"}}}"#;

// Populated token_count event. `total_token_usage` captures the cumulative
// snapshot; `last_token_usage` is the per-turn delta we ingest.
const TURN_A: &str = r#"{"timestamp":"2026-04-14T15:26:26.715Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":24558,"cached_input_tokens":5504,"output_tokens":634,"reasoning_output_tokens":516,"total_tokens":25192},"last_token_usage":{"input_tokens":24558,"cached_input_tokens":5504,"output_tokens":634,"reasoning_output_tokens":516,"total_tokens":25192}}}}"#;

const TURN_B: &str = r#"{"timestamp":"2026-04-14T15:27:12.101Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50974,"cached_input_tokens":8960,"output_tokens":1556,"reasoning_output_tokens":1032,"total_tokens":52530},"last_token_usage":{"input_tokens":26416,"cached_input_tokens":3456,"output_tokens":922,"reasoning_output_tokens":516,"total_tokens":27338}}}}"#;

#[test]
fn session_meta_yields_identity() {
    match parse_codex_line(META_LINE.as_bytes()).unwrap() {
        CodexLine::SessionMeta {
            session_id,
            cwd,
            timestamp,
        } => {
            assert_eq!(session_id, "sess-codex-1");
            assert_eq!(cwd, "/tmp/codex-workspace");
            assert!(timestamp.is_some());
        }
        other => panic!("expected SessionMeta, got {other:?}"),
    }
}

#[test]
fn null_info_rate_limit_pulse_is_classified_as_other() {
    match parse_codex_line(NULL_INFO_LINE.as_bytes()).unwrap() {
        CodexLine::Other => {}
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn token_count_delta_subtracts_cached_from_input() {
    // last_token_usage here has input=24558, cached=5504 → delta.input
    // must be 24558-5504 = 19054. cache_read mirrors cached_input_tokens,
    // cache_creation stays zero (Codex has no separate "creation"
    // concept).
    match parse_codex_line(TURN_A.as_bytes()).unwrap() {
        CodexLine::TokenCount {
            delta, cumulative, ..
        } => {
            assert_eq!(delta.input, 24558 - 5504);
            assert_eq!(delta.output, 634);
            assert_eq!(delta.cache_read, 5504);
            assert_eq!(delta.cache_creation, 0);
            // Cumulative snapshot uses the same subtraction.
            assert_eq!(cumulative.input, 24558 - 5504);
        }
        other => panic!("expected TokenCount, got {other:?}"),
    }
}

#[test]
fn aggregator_ingests_codex_token_counts_via_synthetic_parsed_records() {
    // Scan two populated token_count events and confirm the aggregator
    // counts them as two distinct turns with summed per-turn deltas.
    let mut agg = Aggregator::new();
    for (line, session_id) in [(TURN_A, "sess-codex-1"), (TURN_B, "sess-codex-1")] {
        let CodexLine::TokenCount {
            timestamp, delta, ..
        } = parse_codex_line(line.as_bytes()).unwrap()
        else {
            panic!("expected token_count")
        };
        let rec = parsed_record_from_codex(session_id, "/tmp/codex-workspace", timestamp, delta);
        agg.ingest(&rec);
    }
    let snap = agg.snapshot("codex-workspace", "/tmp/session.jsonl");
    assert_eq!(snap.turns, 2);
    // sum of the two last_token_usage deltas after cached-subtract:
    // (24558-5504) + (26416-3456) = 19054 + 22960 = 42014.
    assert_eq!(snap.cumulative_totals.input, 42014);
    assert_eq!(snap.cumulative_totals.output, 634 + 922);
    assert_eq!(snap.cumulative_totals.cache_read, 5504 + 3456);
    // All codex-origin records carry the synthetic model name.
    let codex = snap
        .by_model
        .iter()
        .find(|m| m.model == CODEX_AGENT_NAME)
        .expect("codex model entry");
    assert_eq!(codex.turns, 2);
}

#[test]
fn unknown_line_types_are_other() {
    match parse_codex_line(
        br#"{"timestamp":"2026-04-14T00:00:00Z","type":"session_footer","payload":{}}"#,
    )
    .unwrap()
    {
        CodexLine::Other => {}
        other => panic!("expected Other for session_footer, got {other:?}"),
    }
    match parse_codex_line(
        br#"{"type":"event_msg","payload":{"type":"agent_message","info":null}}"#,
    )
    .unwrap()
    {
        CodexLine::Other => {}
        other => panic!("expected Other for agent_message, got {other:?}"),
    }
}

#[test]
fn malformed_json_returns_error() {
    assert!(parse_codex_line(b"{ not json").is_err());
}
