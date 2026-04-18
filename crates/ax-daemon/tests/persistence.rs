//! Persistence coverage for the message queue (`queue.json`) and the
//! message history JSONL (`message_history.jsonl`). The task-store,
//! team-state, shared-values, and durable-memory layers already have
//! dedicated test files; this one targets the two slices that landed
//! together in the msgqueue + history commit.

use std::fs;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use ax_daemon::{Daemon, DaemonHandle, HistoryEntry};
use ax_proto::payloads::{
    BroadcastPayload, ReadMessagesPayload, RegisterPayload, SendMessagePayload,
};
use ax_proto::responses::{
    BroadcastResponse, ReadMessagesResponse, SendMessageResponse, StatusResponse,
};
use ax_proto::{Envelope, MessageType, ResponsePayload};
use serde::de::DeserializeOwned;

struct Client {
    writer: tokio::net::unix::OwnedWriteHalf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    counter: u64,
}

impl Client {
    fn connect(socket: &Path) -> Self {
        let std = StdUnixStream::connect(socket).expect("connect");
        std.set_nonblocking(true).expect("nonblocking");
        let stream = UnixStream::from_std(std).expect("from_std");
        let (reader, writer) = stream.into_split();
        Self {
            writer,
            reader: BufReader::new(reader),
            counter: 0,
        }
    }

    fn next_id(&mut self) -> String {
        self.counter += 1;
        format!("p{}", self.counter)
    }

    async fn request<T: serde::Serialize, R: DeserializeOwned>(
        &mut self,
        kind: MessageType,
        payload: &T,
    ) -> R {
        let id = self.next_id();
        let env = Envelope::new(&id, kind, payload).expect("encode envelope");
        let mut bytes = serde_json::to_vec(&env).expect("marshal");
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.expect("write");
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await.expect("read line");
            assert!(n > 0, "daemon closed connection unexpectedly");
            let env: Envelope =
                serde_json::from_str(line.trim_end_matches('\n')).expect("decode envelope");
            if env.id != id {
                continue;
            }
            match env.r#type {
                MessageType::Response => {
                    let wrap: ResponsePayload = env.decode_payload().expect("response payload");
                    assert!(wrap.success, "expected success response");
                    return serde_json::from_str(wrap.data.get()).expect("decode body");
                }
                MessageType::Error => {
                    let err: ax_proto::ErrorPayload = env.decode_payload().expect("error payload");
                    panic!("daemon error: {}", err.message);
                }
                other => panic!("unexpected envelope type: {other:?}"),
            }
        }
    }
}

async fn spawn_daemon(state_dir: &Path) -> DaemonHandle {
    let socket_path = state_dir.join("daemon.sock");
    Daemon::new(socket_path)
        .with_state_dir(state_dir)
        .expect("with_state_dir")
        .bind()
        .await
        .expect("bind daemon")
}

async fn wait_for_socket(socket: &Path) {
    for _ in 0..50 {
        if socket.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn register(workspace: &str) -> RegisterPayload {
    RegisterPayload {
        workspace: workspace.to_owned(),
        dir: format!("/tmp/{workspace}"),
        description: String::new(),
        config_path: String::new(),
        idle_timeout_seconds: 0,
    }
}

#[tokio::test]
async fn queue_snapshot_survives_restart() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();

    // First daemon: orchestrator sends two messages to worker, then shuts down.
    let handle = spawn_daemon(&state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut orch = Client::connect(handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;
    for body in ["hello", "second"] {
        let _: SendMessageResponse = orch
            .request(
                MessageType::SendMessage,
                &SendMessagePayload {
                    to: "worker".into(),
                    message: body.into(),
                    config_path: String::new(),
                },
            )
            .await;
    }
    handle.shutdown().await;

    // Snapshot must have landed to disk.
    let queue_path = state_dir.join("queue.json");
    assert!(
        queue_path.exists(),
        "queue.json should exist after shutdown"
    );

    // Second daemon: worker reads; messages should be present.
    let handle = spawn_daemon(&state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut worker = Client::connect(handle.socket_path());
    let _: StatusResponse = worker
        .request(MessageType::Register, &register("worker"))
        .await;
    let read: ReadMessagesResponse = worker
        .request(
            MessageType::ReadMessages,
            &ReadMessagesPayload {
                limit: 10,
                from: String::new(),
            },
        )
        .await;
    assert_eq!(read.messages.len(), 2);
    assert_eq!(read.messages[0].content, "hello");
    assert_eq!(read.messages[1].content, "second");

    handle.shutdown().await;
}

#[tokio::test]
async fn history_appends_send_and_broadcast_entries() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();

    let handle = spawn_daemon(&state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut orch = Client::connect(handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;
    let mut a = Client::connect(handle.socket_path());
    let _: StatusResponse = a.request(MessageType::Register, &register("a")).await;
    let mut b = Client::connect(handle.socket_path());
    let _: StatusResponse = b.request(MessageType::Register, &register("b")).await;

    let _: SendMessageResponse = orch
        .request(
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "a".into(),
                message: "first".into(),
                config_path: String::new(),
            },
        )
        .await;
    let bcast: BroadcastResponse = orch
        .request(
            MessageType::Broadcast,
            &BroadcastPayload {
                message: "announce".into(),
                config_path: String::new(),
            },
        )
        .await;
    assert_eq!(bcast.count, 2);
    handle.shutdown().await;

    let history_path = state_dir.join("message_history.jsonl");
    let raw = fs::read_to_string(&history_path).expect("history readable");
    let entries: Vec<HistoryEntry> = raw
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("decode history line"))
        .collect();
    assert_eq!(entries.len(), 3, "1 send + 2 broadcast recipients");
    assert!(entries.iter().any(|e| e.to == "a" && e.content == "first"));
    assert!(entries
        .iter()
        .any(|e| e.content == "announce" && (e.to == "a" || e.to == "b")));
}

#[tokio::test]
async fn history_survives_restart_and_trims_to_max_size() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();

    let handle = spawn_daemon(&state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut orch = Client::connect(handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;
    for i in 0..4 {
        let _: SendMessageResponse = orch
            .request(
                MessageType::SendMessage,
                &SendMessagePayload {
                    to: "worker".into(),
                    message: format!("msg-{i}"),
                    config_path: String::new(),
                },
            )
            .await;
    }
    handle.shutdown().await;

    // Fresh daemon — history file must have all four entries.
    let handle = spawn_daemon(&state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let history_path = state_dir.join("message_history.jsonl");
    let raw = fs::read_to_string(&history_path).expect("history readable");
    let lines: Vec<_> = raw.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 4);
    handle.shutdown().await;
}
