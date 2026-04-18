//! End-to-end `usage_trends` request: seed a Claude transcript + Codex
//! session on disk, point HOME at the temp dir, and verify the daemon
//! returns a `UsageTrendsResponse` whose numbers match the seeded data.

use std::fs;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use ax_daemon::Daemon;
use ax_proto::payloads::{UsageTrendWorkspace, UsageTrendsPayload};
use ax_proto::responses::UsageTrendsResponse;
use ax_proto::{Envelope, MessageType, ResponsePayload};

struct Client {
    writer: OwnedWriteHalf,
    reader: BufReader<OwnedReadHalf>,
}

async fn connect(path: &Path) -> Client {
    let stream = UnixStream::connect(path).await.unwrap();
    let (rh, wh) = stream.into_split();
    Client {
        writer: wh,
        reader: BufReader::new(rh),
    }
}

async fn send_envelope(writer: &mut OwnedWriteHalf, env: &Envelope) {
    let mut bytes = serde_json::to_vec(env).unwrap();
    bytes.push(b'\n');
    writer.write_all(&bytes).await.unwrap();
}

async fn await_response(reader: &mut BufReader<OwnedReadHalf>, id: &str) -> Envelope {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        assert!(n > 0);
        let env: Envelope = serde_json::from_str(line.trim_end()).unwrap();
        if env.id == id {
            return env;
        }
    }
}

fn decode_response<T: for<'de> serde::Deserialize<'de>>(env: &Envelope) -> T {
    assert_eq!(env.r#type, MessageType::Response);
    let wrap: ResponsePayload = env.decode_payload().unwrap();
    assert!(wrap.success);
    serde_json::from_str(wrap.data.get()).unwrap()
}

fn write_file(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn claude_turn(req: &str, ts: &str, input: i64, output: i64, cwd: &str) -> String {
    format!(
        r#"{{"type":"assistant","sessionId":"sess-{req}","cwd":"{cwd}","timestamp":"{ts}","requestId":"{req}","message":{{"id":"msg-{req}","role":"assistant","model":"claude-opus-4-7","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"content":[{{"type":"text","text":"hi"}}]}}}}
"#,
    )
}

fn codex_session(session_id: &str, cwd: &str, ts: &str, input: i64, output: i64) -> String {
    let total = input + output;
    format!(
        r#"{{"timestamp":"{ts}","type":"session_meta","payload":{{"id":"{session_id}","timestamp":"{ts}","cwd":"{cwd}"}}}}
{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":0,"output_tokens":{output},"reasoning_output_tokens":0,"total_tokens":{total}}},"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":0,"output_tokens":{output},"reasoning_output_tokens":0,"total_tokens":{total}}}}}}}}}
"#,
    )
}

/// Claude's "encode project key" turns `/a/b` into `-a-b`. Mirror it so
/// the test seeds to the same dir ax-agent will resolve.
fn claude_project_subdir(cwd: &str) -> String {
    cwd.replace(['/', '.'], "-")
}

fn codex_subdir(workspace: &str, cwd: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(cwd.as_bytes());
    let digest = hasher.finalize();
    format!("{workspace}-{}", hex::encode(&digest[..6]))
}

// Tests in this file mutate `HOME` so that ax-agent's path helpers
// resolve under the temp dir. Run in-process sequentially by keeping
// every assertion in a single #[tokio::test] — the `cargo test` default
// parallelism would otherwise race on the shared env.
#[tokio::test]
async fn usage_trends_populates_totals_and_unavailable_reasons() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());

    let workspace_name = "worker";
    let workspace_cwd = "/tmp/demo-ws";

    // Claude: seed a transcript at ~/.claude/projects/<encoded>/chat.jsonl
    let claude_root = home
        .path()
        .join(".claude")
        .join("projects")
        .join(claude_project_subdir(workspace_cwd));
    // Pin the timestamp to "now" so the 3-hour default window catches it.
    let ts = chrono::Utc::now();
    let ts_str = ts.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    write_file(
        &claude_root.join("chat.jsonl"),
        &claude_turn("req-c1", &ts_str, 1000, 200, workspace_cwd),
    );

    // Codex: seed a session under $HOME/.ax/codex/<workspace-hash>/sessions/YYYY/MM/DD/
    let codex_root = home
        .path()
        .join(".ax")
        .join("codex")
        .join(codex_subdir(workspace_name, workspace_cwd));
    let codex_sessions = codex_root
        .join("sessions")
        .join(format!("{}", ts.format("%Y")))
        .join(format!("{}", ts.format("%m")))
        .join(format!("{}", ts.format("%d")));
    write_file(
        &codex_sessions.join("rollout-1.jsonl"),
        &codex_session("codex-sess-1", workspace_cwd, &ts_str, 500, 80),
    );

    let sock_dir: PathBuf = home.path().join("run");
    fs::create_dir_all(&sock_dir).unwrap();
    let socket = sock_dir.join("ax.sock");
    let handle = Daemon::new(socket.clone()).bind().await.unwrap();

    let mut client = connect(&socket).await;
    // Request two bindings: one seeded, one pointing at an unseeded cwd.
    let env = Envelope::new(
        "req-trends",
        MessageType::UsageTrends,
        &UsageTrendsPayload {
            workspaces: vec![
                UsageTrendWorkspace {
                    workspace: workspace_name.into(),
                    cwd: workspace_cwd.into(),
                },
                UsageTrendWorkspace {
                    workspace: "ghost".into(),
                    cwd: "/tmp/nothing-here".into(),
                },
            ],
            since_minutes: 180,
            bucket_minutes: 5,
        },
    )
    .unwrap();
    send_envelope(&mut client.writer, &env).await;
    let resp: UsageTrendsResponse =
        decode_response(&await_response(&mut client.reader, "req-trends").await);

    assert_eq!(resp.trends.len(), 2);
    let worker = resp
        .trends
        .iter()
        .find(|t| t.workspace == workspace_name)
        .expect("worker trend");
    assert_eq!(worker.cwd, workspace_cwd);
    assert!(worker.available, "reason={}", worker.unavailable_reason);
    assert_eq!(worker.total.input, 1000 + 500);
    assert_eq!(worker.total.output, 200 + 80);
    assert_eq!(worker.bucket_minutes, 5);
    let agent_names: Vec<&str> = worker.agents.iter().map(|a| a.agent.as_str()).collect();
    assert!(agent_names.contains(&"main"));
    assert!(agent_names.contains(&"codex"));

    let ghost = resp
        .trends
        .iter()
        .find(|t| t.workspace == "ghost")
        .expect("ghost trend");
    assert!(!ghost.available);
    assert_eq!(ghost.unavailable_reason, "no_project_transcripts");
    // `unavailable_reason` is mirrored into `error` for unavailable workspaces.
    assert_eq!(ghost.error, "no_project_transcripts");

    handle.shutdown().await;
}
