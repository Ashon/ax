//! End-to-end scan: build a synthetic Claude project directory, drop
//! transcripts into it, and verify the bucket/agent rollups.

use std::fs;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};

use ax_usage::{discover_transcripts, scan_workspace_from_project_dir, HistoryQuery};

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn turn(timestamp: &str, request_id: &str, input: i64, output: i64) -> String {
    format!(
        r#"{{"type":"assistant","sessionId":"sess-1","cwd":"/tmp/proj","timestamp":"{timestamp}","requestId":"{request_id}","message":{{"id":"msg-{request_id}","role":"assistant","model":"claude-opus-4-7","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"content":[{{"type":"text","text":"hi"}}]}}}}
"#,
    )
}

fn default_window() -> HistoryQuery {
    HistoryQuery {
        since: Utc.with_ymd_and_hms(2026, 4, 14, 0, 0, 0).unwrap(),
        until: Utc.with_ymd_and_hms(2026, 4, 18, 0, 0, 0).unwrap(),
        bucket_size: Duration::from_secs(60 * 60), // 1h buckets
    }
}

#[test]
fn missing_dir_reports_missing_workspace_dir() {
    let result = scan_workspace_from_project_dir(
        "worker",
        "",
        Path::new("/definitely/not/there"),
        &default_window(),
    )
    .unwrap();
    assert!(!result.available);
    assert_eq!(result.unavailable_reason, "missing_workspace_dir");
}

#[test]
fn missing_project_dir_reports_no_project_transcripts() {
    let result = scan_workspace_from_project_dir(
        "worker",
        "/some/dir",
        Path::new("/definitely/not/there"),
        &default_window(),
    )
    .unwrap();
    assert!(!result.available);
    assert_eq!(result.unavailable_reason, "no_project_transcripts");
}

#[test]
fn empty_project_dir_reports_no_transcripts() {
    let dir = tempfile::tempdir().unwrap();
    let result =
        scan_workspace_from_project_dir("worker", "/some/dir", dir.path(), &default_window())
            .unwrap();
    assert!(!result.available);
    assert_eq!(result.unavailable_reason, "no_transcripts");
}

#[test]
fn one_transcript_rolls_up_into_main_agent_with_buckets() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("chat-abc.jsonl");
    let mut body = String::new();
    body.push_str(&turn("2026-04-15T10:00:00Z", "req-1", 1000, 200));
    body.push_str(&turn("2026-04-15T10:30:00Z", "req-2", 2000, 400));
    body.push_str(&turn("2026-04-15T11:00:00Z", "req-3", 500, 100));
    write(&transcript, &body);

    let result =
        scan_workspace_from_project_dir("worker", "/some/dir", dir.path(), &default_window())
            .unwrap();
    assert!(result.available, "reason={:?}", result.unavailable_reason);
    assert_eq!(result.agents.len(), 1);
    let agent = &result.agents[0];
    assert_eq!(agent.agent, "main");
    assert_eq!(agent.current_snapshot.turns, 3);
    // Cumulative across three turns.
    assert_eq!(agent.current_snapshot.cumulative_totals.input, 3500);
    assert_eq!(agent.current_snapshot.cumulative_totals.output, 700);
    // Two buckets (10:00 and 11:00 wall hours).
    assert_eq!(
        agent.recent_buckets.len(),
        2,
        "buckets = {:?}",
        agent.recent_buckets
    );
    // Workspace rollup matches the agent's buckets since there's only one.
    assert_eq!(result.recent_buckets.len(), 2);
    assert_eq!(result.current_snapshot.cumulative_totals.input, 3500);
}

#[test]
fn records_outside_query_window_are_excluded_from_buckets() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("chat-out.jsonl");
    let body = format!(
        "{}\n{}",
        turn("2026-04-10T10:00:00Z", "req-a", 1000, 0), // outside window
        turn("2026-04-15T10:00:00Z", "req-b", 500, 0),
    );
    write(&transcript, &body);

    let mut q = default_window();
    q.since = Utc.with_ymd_and_hms(2026, 4, 14, 0, 0, 0).unwrap();
    q.until = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();

    let result = scan_workspace_from_project_dir("worker", "/some/dir", dir.path(), &q).unwrap();
    let agent = &result.agents[0];
    // Both turns show up in cumulative totals (scan reads the full file)
    // but only the in-window one produces a bucket entry.
    assert_eq!(agent.recent_buckets.len(), 1);
    assert_eq!(agent.recent_buckets[0].tokens.input, 500);
}

#[test]
fn discover_transcripts_returns_jsonl_only_sorted() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a.jsonl"), "");
    write(&dir.path().join("b.txt"), "");
    write(&dir.path().join("nested/c.jsonl"), "");

    let found = discover_transcripts(dir.path()).unwrap();
    let names: Vec<String> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"a.jsonl".to_owned()));
    assert!(names.contains(&"c.jsonl".to_owned()));
    assert!(!names.contains(&"b.txt".to_owned()));
    // Sort stability.
    let mut sorted = found.clone();
    sorted.sort();
    assert_eq!(found, sorted);
}

#[test]
fn normalize_fills_in_defaults_for_zero_values() {
    let epoch = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
    let q = HistoryQuery {
        since: epoch,
        until: epoch,
        bucket_size: Duration::from_secs(0),
    };
    let now = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let normalized = q.normalized(now);
    assert_eq!(normalized.until, now);
    assert_eq!(normalized.bucket_size, ax_usage::DEFAULT_BUCKET_SIZE);
    // since should be 3h before `until`.
    let span = normalized.until - normalized.since;
    assert_eq!(span, chrono::Duration::hours(3));
}
