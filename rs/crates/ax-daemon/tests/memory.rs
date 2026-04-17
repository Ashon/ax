//! End-to-end memory-handler scenarios. The test registers a
//! workspace, stores a handful of memories across scopes/kinds, then
//! replays supersede and scope-filter semantics through the daemon.

use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use ax_daemon::Daemon;
use ax_proto::payloads::{RecallMemoriesPayload, RegisterPayload, RememberMemoryPayload};
use ax_proto::responses::{MemoryResponse, RecallMemoriesResponse, StatusResponse};
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

async fn send(writer: &mut OwnedWriteHalf, env: &Envelope) {
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

fn decode<T: for<'de> serde::Deserialize<'de>>(env: &Envelope) -> T {
    let wrap: ResponsePayload = env.decode_payload().unwrap();
    assert!(wrap.success);
    serde_json::from_str(wrap.data.get()).unwrap()
}

async fn register(client: &mut Client, name: &str) {
    let id = format!("req-register-{name}");
    let env = Envelope::new(
        &id,
        MessageType::Register,
        &RegisterPayload {
            workspace: name.into(),
            dir: "/tmp/ws".into(),
            description: String::new(),
            config_path: String::new(),
            idle_timeout_seconds: 0,
        },
    )
    .unwrap();
    send(&mut client.writer, &env).await;
    let _: StatusResponse = decode(&await_response(&mut client.reader, &id).await);
}

async fn remember(
    client: &mut Client,
    req_id: &str,
    scope: &str,
    kind: &str,
    content: &str,
    tags: &[&str],
    supersedes: &[&str],
) -> MemoryResponse {
    let env = Envelope::new(
        req_id,
        MessageType::RememberMemory,
        &RememberMemoryPayload {
            scope: scope.into(),
            kind: kind.into(),
            subject: String::new(),
            content: content.into(),
            tags: tags.iter().map(ToString::to_string).collect(),
            supersedes: supersedes.iter().map(ToString::to_string).collect(),
        },
    )
    .unwrap();
    send(&mut client.writer, &env).await;
    decode(&await_response(&mut client.reader, req_id).await)
}

async fn recall(
    client: &mut Client,
    req_id: &str,
    scopes: &[&str],
    kind: &str,
    include_superseded: bool,
) -> RecallMemoriesResponse {
    let env = Envelope::new(
        req_id,
        MessageType::RecallMemories,
        &RecallMemoriesPayload {
            scopes: scopes.iter().map(ToString::to_string).collect(),
            kind: kind.into(),
            tags: Vec::new(),
            include_superseded,
            limit: 0,
        },
    )
    .unwrap();
    send(&mut client.writer, &env).await;
    decode(&await_response(&mut client.reader, req_id).await)
}

#[tokio::test]
async fn remember_then_recall_filters_scope_and_applies_supersede() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("ax.sock");
    let state = tmp.path().join("state");

    let handle = Daemon::new(socket.clone())
        .with_state_dir(&state)
        .unwrap()
        .bind()
        .await
        .unwrap();

    let mut client = connect(&socket).await;
    register(&mut client, "worker").await;

    // Seed three memories: two in project:ax, one in workspace:worker.
    let orig = remember(
        &mut client,
        "req-r1",
        "project:ax",
        "decision",
        "migration: Rust first, Go second",
        &["rust"],
        &[],
    )
    .await;
    let _ = remember(
        &mut client,
        "req-r2",
        "project:ax",
        "decision",
        "protocol is wire-compat with Go",
        &[],
        &[],
    )
    .await;
    let _ = remember(
        &mut client,
        "req-r3",
        "workspace:worker",
        "fact",
        "worker is the demo agent",
        &[],
        &[],
    )
    .await;

    // Supersede the first memory with a revised version.
    let _ = remember(
        &mut client,
        "req-r4",
        "project:ax",
        "decision",
        "migration: Rust only; delete Go after cutover",
        &["rust"],
        &[orig.memory.id.as_str()],
    )
    .await;

    // Default recall excludes superseded — we see 2 project:ax + 1 workspace:worker.
    let all = recall(&mut client, "req-list-all", &[], "", false).await;
    assert_eq!(all.memories.len(), 3);
    // The first memory is gone from the default view.
    assert!(!all.memories.iter().any(|m| m.id == orig.memory.id));

    // include_superseded=true restores it.
    let all_super = recall(&mut client, "req-list-all-super", &[], "", true).await;
    assert_eq!(all_super.memories.len(), 4);
    let superseded = all_super
        .memories
        .iter()
        .find(|m| m.id == orig.memory.id)
        .expect("the original entry still exists");
    assert!(superseded.superseded_at.is_some());
    assert!(!superseded.superseded_by.is_empty());

    // Scope filter: project:ax → only the two project decisions (one
    // superseded, so only the replacement + the unrelated one).
    let project = recall(&mut client, "req-list-proj", &["project:ax"], "", false).await;
    assert_eq!(project.memories.len(), 2);
    for m in &project.memories {
        assert_eq!(m.scope, "project:ax");
    }

    // Workspace-scoped recall.
    let ws = recall(&mut client, "req-list-ws", &["workspace:worker"], "", false).await;
    assert_eq!(ws.memories.len(), 1);
    assert_eq!(ws.memories[0].scope, "workspace:worker");

    handle.shutdown().await;
}

#[tokio::test]
async fn remember_persists_across_restart_and_superseded_flag_sticks() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let socket_a = tmp.path().join("a.sock");
    let orig_id: String;

    {
        let handle = Daemon::new(socket_a.clone())
            .with_state_dir(&state)
            .unwrap()
            .bind()
            .await
            .unwrap();
        let mut client = connect(&socket_a).await;
        register(&mut client, "worker").await;
        let a = remember(
            &mut client,
            "req-r1",
            "global",
            "decision",
            "v1 behaviour",
            &[],
            &[],
        )
        .await;
        orig_id = a.memory.id.clone();
        let _ = remember(
            &mut client,
            "req-r2",
            "global",
            "decision",
            "v2 behaviour",
            &[],
            &[orig_id.as_str()],
        )
        .await;
        handle.shutdown().await;
    }

    let socket_b = tmp.path().join("b.sock");
    let handle = Daemon::new(socket_b.clone())
        .with_state_dir(&state)
        .unwrap()
        .bind()
        .await
        .unwrap();
    let mut client = connect(&socket_b).await;
    register(&mut client, "worker").await;
    let all_super = recall(&mut client, "req-list-all", &["global"], "", true).await;
    assert_eq!(all_super.memories.len(), 2);
    let old = all_super
        .memories
        .iter()
        .find(|m| m.id == orig_id)
        .expect("v1 survives the restart");
    assert!(old.superseded_at.is_some());
    handle.shutdown().await;
}
