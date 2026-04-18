//! Multi-binding `query_history` + Codex integration + `query_workspace_trends`.
//!
//! Exercises the three attribution heuristics Go uses (hint, session id,
//! unique cwd) plus the Codex session stitching.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{TimeZone, Utc};

use ax_usage::{query_history, query_workspace_trends, HistoryQuery, WorkspaceBinding};

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn default_query() -> HistoryQuery {
    HistoryQuery {
        since: Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap(),
        until: Utc.with_ymd_and_hms(2026, 4, 30, 0, 0, 0).unwrap(),
        bucket_size: Duration::from_secs(60 * 60),
    }
}

// Attached workspace hint extractor uses the Go prefix verbatim.
fn turn(req: &str, ts: &str, input: i64, output: i64, hint: Option<&str>) -> String {
    let hint_line = match hint {
        Some(name) => format!(
            r#"{{"type":"attachment","sessionId":"sess-{req}","timestamp":"{ts}","attachment":{{"type":"mcp_instructions_delta","addedBlocks":["You are the \"{name}\" workspace agent in an ax multi-agent environment."]}}}}
"#,
        ),
        None => String::new(),
    };
    let assistant = format!(
        r#"{{"type":"assistant","sessionId":"sess-{req}","cwd":"/tmp/claude-proj","timestamp":"{ts}","requestId":"{req}","message":{{"id":"msg-{req}","role":"assistant","model":"claude-opus-4-7","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"content":[{{"type":"text","text":"hi"}}]}}}}
"#
    );
    format!("{hint_line}{assistant}")
}

fn codex_session(session_id: &str, cwd: &str, ts: &str, input: i64, output: i64) -> String {
    let total = input + output;
    format!(
        r#"{{"timestamp":"{ts}","type":"session_meta","payload":{{"id":"{session_id}","timestamp":"{ts}","cwd":"{cwd}"}}}}
{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":0,"output_tokens":{output},"reasoning_output_tokens":0,"total_tokens":{total}}},"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":0,"output_tokens":{output},"reasoning_output_tokens":0,"total_tokens":{total}}}}}}}}}
"#,
    )
}

#[test]
fn claude_hint_wins_over_unique_cwd_on_shared_dir() {
    // Two bindings share the same project dir. The transcript carries a
    // workspace hint that identifies `worker-b`, so attribution must
    // route to b, not a.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("claude_projects").join("proj");
    let transcript = project.join("chat.jsonl");
    let mut body = String::new();
    body.push_str(&turn(
        "req1",
        "2026-04-15T10:00:00Z",
        1000,
        200,
        Some("worker-b"),
    ));
    write(&transcript, &body);

    let bindings = vec![
        WorkspaceBinding {
            name: "worker-a".into(),
            dir: "/tmp/claude-proj".into(),
            claude_project_dir: Some(project.clone()),
            ..Default::default()
        },
        WorkspaceBinding {
            name: "worker-b".into(),
            dir: "/tmp/claude-proj".into(),
            claude_project_dir: Some(project.clone()),
            ..Default::default()
        },
    ];
    let resp = query_history(&bindings, &default_query()).unwrap();
    let a = resp
        .workspaces
        .iter()
        .find(|w| w.workspace == "worker-a")
        .unwrap();
    let b = resp
        .workspaces
        .iter()
        .find(|w| w.workspace == "worker-b")
        .unwrap();
    assert!(!a.available, "worker-a should not absorb hinted series");
    assert!(b.available, "worker-b must receive the hinted series");
    assert_eq!(b.current_snapshot.cumulative_totals.input, 1000);
}

#[test]
fn unique_cwd_fallback_attributes_hintless_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("claude_projects").join("uniq");
    let transcript = project.join("chat.jsonl");
    let body = turn("req1", "2026-04-15T10:00:00Z", 800, 100, None);
    write(&transcript, &body);

    let bindings = vec![WorkspaceBinding {
        name: "solo".into(),
        dir: "/tmp/claude-proj".into(),
        claude_project_dir: Some(project.clone()),
        ..Default::default()
    }];
    let resp = query_history(&bindings, &default_query()).unwrap();
    let ws = &resp.workspaces[0];
    assert!(ws.available);
    assert_eq!(ws.current_snapshot.cumulative_totals.input, 800);
}

#[test]
fn codex_series_merges_into_workspace_history() {
    let tmp = tempfile::tempdir().unwrap();
    let codex_home = tmp.path().join("codex-home");
    let sessions = codex_home
        .join("sessions")
        .join("2026")
        .join("04")
        .join("15");
    write(
        &sessions.join("rollout-1.jsonl"),
        &codex_session(
            "codex-sess-1",
            "/tmp/codex-workspace",
            "2026-04-15T10:00:00Z",
            500,
            80,
        ),
    );

    let bindings = vec![WorkspaceBinding {
        name: "codex-only".into(),
        dir: "/tmp/codex-workspace".into(),
        claude_project_dir: None,
        codex_home: Some(codex_home.clone()),
    }];
    let resp = query_history(&bindings, &default_query()).unwrap();
    let ws = &resp.workspaces[0];
    assert!(ws.available, "reason={:?}", ws.unavailable_reason);
    let codex_agent = ws.agents.iter().find(|a| a.agent == "codex").unwrap();
    assert_eq!(codex_agent.current_snapshot.cumulative_totals.input, 500);
    assert_eq!(codex_agent.current_snapshot.cumulative_totals.output, 80);
}

#[test]
fn mixed_claude_and_codex_produces_two_agents() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_proj = tmp.path().join("claude_projects").join("proj");
    write(
        &claude_proj.join("chat.jsonl"),
        &turn("req1", "2026-04-15T10:00:00Z", 1000, 200, Some("mixed")),
    );
    let codex_home = tmp.path().join("codex-home");
    let sessions = codex_home
        .join("sessions")
        .join("2026")
        .join("04")
        .join("15");
    write(
        &sessions.join("rollout-1.jsonl"),
        &codex_session(
            "codex-sess-1",
            "/tmp/claude-proj",
            "2026-04-15T10:30:00Z",
            500,
            80,
        ),
    );

    let bindings = vec![WorkspaceBinding {
        name: "mixed".into(),
        dir: "/tmp/claude-proj".into(),
        claude_project_dir: Some(claude_proj),
        codex_home: Some(codex_home),
    }];
    let resp = query_history(&bindings, &default_query()).unwrap();
    let ws = &resp.workspaces[0];
    assert!(ws.available);
    // Both the Claude "main" and Codex series are rolled up.
    let agents: Vec<&str> = ws.agents.iter().map(|a| a.agent.as_str()).collect();
    assert!(agents.contains(&"main"));
    assert!(agents.contains(&"codex"));
    // Cumulative sum spans both runtimes.
    assert_eq!(ws.current_snapshot.cumulative_totals.input, 1000 + 500);
}

#[test]
fn query_workspace_trends_reshapes_into_proto_type() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_proj = tmp.path().join("claude_projects").join("trend");
    write(
        &claude_proj.join("chat.jsonl"),
        &turn("req1", "2026-04-15T10:00:00Z", 1000, 200, Some("trendy")),
    );

    let bindings = vec![WorkspaceBinding {
        name: "trendy".into(),
        dir: "/tmp/claude-proj".into(),
        claude_project_dir: Some(claude_proj),
        ..Default::default()
    }];
    let resp = query_history(&bindings, &default_query()).unwrap();
    let trends = query_workspace_trends(&resp);
    assert_eq!(trends.len(), 1);
    let t = &trends[0];
    assert_eq!(t.workspace, "trendy");
    assert!(t.available);
    assert_eq!(t.bucket_minutes, 60);
    assert_eq!(t.total.input, 1000);
    assert_eq!(t.total.output, 200);
    assert!(!t.buckets.is_empty());
    // Agents list mirrors the WorkspaceHistory entries.
    assert!(t.agents.iter().any(|a| a.agent == "main"));
}

#[test]
fn unavailable_reasons_reflect_runtime_visibility() {
    let tmp = tempfile::tempdir().unwrap();

    // Empty dir case.
    let empty_case = vec![WorkspaceBinding {
        name: "no-data".into(),
        dir: "/tmp/claude-proj".into(),
        claude_project_dir: Some(tmp.path().join("missing_dir")),
        codex_home: None,
    }];
    let resp = query_history(&empty_case, &default_query()).unwrap();
    assert_eq!(
        resp.workspaces[0].unavailable_reason,
        "no_project_transcripts"
    );

    // Claude dir exists but empty.
    let present_empty = tmp.path().join("present_empty");
    fs::create_dir_all(&present_empty).unwrap();
    let bindings = vec![WorkspaceBinding {
        name: "present".into(),
        dir: "/tmp/claude-proj".into(),
        claude_project_dir: Some(present_empty),
        codex_home: None,
    }];
    let resp = query_history(&bindings, &default_query()).unwrap();
    assert_eq!(resp.workspaces[0].unavailable_reason, "no_transcripts");
}

// Helpers —— keep a noop reference to PathBuf so rustfmt doesn't drop
// the import on re-format.
#[allow(dead_code)]
fn _touch(_: PathBuf) {}
