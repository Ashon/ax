//! End-to-end coverage for wake-scheduler wiring inside the running
//! daemon. Unit tests for the state machine live alongside the module;
//! this file validates that `send_message` / `broadcast` actually
//! register pending wakes and that `read_messages` clears them, both
//! via the external Unix-socket surface.

use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use ax_daemon::{Daemon, DaemonHandle, WakeScheduler};
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
        format!("w{}", self.counter)
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

fn register(workspace: &str) -> RegisterPayload {
    RegisterPayload {
        workspace: workspace.to_owned(),
        dir: format!("/tmp/{workspace}"),
        description: String::new(),
        config_path: String::new(),
        idle_timeout_seconds: 0,
    }
}

struct Fixture {
    _tmp: TempDir,
    daemon: Daemon,
    handle: DaemonHandle,
}

async fn spawn() -> Fixture {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();
    let socket = state_dir.join("daemon.sock");
    let daemon = Daemon::new(socket)
        .with_state_dir(&state_dir)
        .expect("state");
    let handle = daemon.clone().bind().await.expect("bind");
    // Wait until the socket is accepting connections.
    for _ in 0..50 {
        if handle.socket_path().exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Fixture {
        _tmp: tmp,
        daemon,
        handle,
    }
}

#[tokio::test]
async fn send_message_schedules_a_pending_wake() {
    let f = spawn().await;
    let mut orch = Client::connect(f.handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;
    let _: SendMessageResponse = orch
        .request(
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "worker".into(),
                message: "please do X".into(),
                config_path: String::new(),
            },
        )
        .await;
    let scheduler: &WakeScheduler = &f.daemon.wake_scheduler;
    let state = scheduler.state("worker").expect("wake should be scheduled");
    assert_eq!(state.workspace, "worker");
    assert_eq!(state.sender, "orch");
    assert_eq!(state.attempts, 0);
    f.handle.shutdown().await;
}

#[tokio::test]
async fn broadcast_schedules_wakes_for_every_recipient() {
    let f = spawn().await;
    let mut orch = Client::connect(f.handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;
    let mut a = Client::connect(f.handle.socket_path());
    let _: StatusResponse = a.request(MessageType::Register, &register("a")).await;
    let mut b = Client::connect(f.handle.socket_path());
    let _: StatusResponse = b.request(MessageType::Register, &register("b")).await;

    let resp: BroadcastResponse = orch
        .request(
            MessageType::Broadcast,
            &BroadcastPayload {
                message: "global".into(),
                config_path: String::new(),
            },
        )
        .await;
    assert_eq!(resp.count, 2);

    let scheduler: &WakeScheduler = &f.daemon.wake_scheduler;
    assert!(scheduler.state("a").is_some());
    assert!(scheduler.state("b").is_some());
    assert!(
        scheduler.state("orch").is_none(),
        "self should not be woken"
    );
    f.handle.shutdown().await;
}

#[tokio::test]
async fn read_messages_cancels_pending_wake_when_inbox_drains() {
    let f = spawn().await;
    let mut orch = Client::connect(f.handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;
    let mut worker = Client::connect(f.handle.socket_path());
    let _: StatusResponse = worker
        .request(MessageType::Register, &register("worker"))
        .await;
    let _: SendMessageResponse = orch
        .request(
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "worker".into(),
                message: "m1".into(),
                config_path: String::new(),
            },
        )
        .await;

    let scheduler: &WakeScheduler = &f.daemon.wake_scheduler;
    assert!(scheduler.state("worker").is_some());

    let drained: ReadMessagesResponse = worker
        .request(
            MessageType::ReadMessages,
            &ReadMessagesPayload {
                limit: 10,
                from: String::new(),
            },
        )
        .await;
    assert_eq!(drained.messages.len(), 1);
    assert!(
        scheduler.state("worker").is_none(),
        "empty inbox should cancel the wake"
    );
    f.handle.shutdown().await;
}
