//! End-to-end test for the MVP handler set. Spins up an in-process
//! daemon bound to a temp Unix socket, opens two client connections,
//! and verifies `register` → `send_message` → push + `read_messages` +
//! `broadcast` + `list_workspaces` + `set_status`.

use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use ax_daemon::Daemon;
use ax_proto::payloads::{
    BroadcastPayload, ReadMessagesPayload, RegisterPayload, SendMessagePayload, SetStatusPayload,
};
use ax_proto::responses::{
    BroadcastResponse, ListWorkspacesResponse, ReadMessagesResponse, SendMessageResponse,
    StatusResponse,
};
use ax_proto::types::Message;
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

async fn read_envelope(reader: &mut BufReader<OwnedReadHalf>) -> Envelope {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await.unwrap();
    assert!(n > 0, "EOF before envelope arrived");
    serde_json::from_str(line.trim_end()).unwrap()
}

async fn await_response(reader: &mut BufReader<OwnedReadHalf>, id: &str) -> Envelope {
    // Drain push envelopes until we find the response keyed by `id`.
    loop {
        let env = read_envelope(reader).await;
        if env.id == id {
            return env;
        }
    }
}

fn decode_response<T: for<'de> serde::Deserialize<'de>>(env: &Envelope) -> T {
    assert_eq!(
        env.r#type,
        MessageType::Response,
        "expected Response, got {:?}",
        env.r#type,
    );
    let wrap: ResponsePayload = env.decode_payload().unwrap();
    assert!(wrap.success);
    serde_json::from_str(wrap.data.get()).unwrap()
}

async fn register(client: &mut Client, name: &str, dir: &str) {
    let id = format!("req-register-{name}");
    let env = Envelope::new(
        &id,
        MessageType::Register,
        &RegisterPayload {
            workspace: name.into(),
            dir: dir.into(),
            description: String::new(),
            config_path: String::new(),
            idle_timeout_seconds: 0,
        },
    )
    .unwrap();
    send_envelope(&mut client.writer, &env).await;
    let _: StatusResponse = decode_response(&await_response(&mut client.reader, &id).await);
}

#[tokio::test]
async fn register_send_read_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("ax.sock");
    let handle = Daemon::new(socket.clone()).bind().await.unwrap();

    let mut alice = connect(&socket).await;
    let mut bob = connect(&socket).await;
    register(&mut alice, "alice", "/tmp/a").await;
    register(&mut bob, "bob", "/tmp/b").await;

    let env = Envelope::new(
        "req-send-1",
        MessageType::SendMessage,
        &SendMessagePayload {
            to: "bob".into(),
            message: "hello".into(),
            config_path: String::new(),
        },
    )
    .unwrap();
    send_envelope(&mut alice.writer, &env).await;
    let resp: SendMessageResponse =
        decode_response(&await_response(&mut alice.reader, "req-send-1").await);
    assert_eq!(resp.status, "sent");
    assert!(!resp.message_id.is_empty());

    let push = read_envelope(&mut bob.reader).await;
    assert_eq!(push.r#type, MessageType::PushMessage);
    let msg: Message = push.decode_payload().unwrap();
    assert_eq!(msg.from, "alice");
    assert_eq!(msg.to, "bob");
    assert_eq!(msg.content, "hello");
    assert_eq!(msg.id, resp.message_id);

    let env = Envelope::new(
        "req-read-1",
        MessageType::ReadMessages,
        &ReadMessagesPayload {
            limit: 10,
            from: String::new(),
        },
    )
    .unwrap();
    send_envelope(&mut bob.writer, &env).await;
    let drained: ReadMessagesResponse =
        decode_response(&await_response(&mut bob.reader, "req-read-1").await);
    assert_eq!(drained.messages.len(), 1);
    assert_eq!(drained.messages[0].content, "hello");

    handle.shutdown().await;
}

#[tokio::test]
async fn broadcast_reaches_every_peer_except_sender() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("ax.sock");
    let handle = Daemon::new(socket.clone()).bind().await.unwrap();

    let mut alice = connect(&socket).await;
    let mut bob = connect(&socket).await;
    let mut carol = connect(&socket).await;
    register(&mut alice, "alice", "/tmp/a").await;
    register(&mut bob, "bob", "/tmp/b").await;
    register(&mut carol, "carol", "/tmp/c").await;

    let env = Envelope::new(
        "req-bcast",
        MessageType::Broadcast,
        &BroadcastPayload {
            message: "heads up".into(),
            config_path: String::new(),
        },
    )
    .unwrap();
    send_envelope(&mut alice.writer, &env).await;
    let resp: BroadcastResponse =
        decode_response(&await_response(&mut alice.reader, "req-bcast").await);
    assert_eq!(resp.count, 2);
    let mut sorted = resp.recipients.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["bob".to_string(), "carol".to_string()]);

    for reader in [&mut bob.reader, &mut carol.reader] {
        let push = read_envelope(reader).await;
        assert_eq!(push.r#type, MessageType::PushMessage);
        let msg: Message = push.decode_payload().unwrap();
        assert_eq!(msg.from, "alice");
        assert_eq!(msg.content, "heads up");
    }

    handle.shutdown().await;
}

#[tokio::test]
async fn list_workspaces_reflects_status_text_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("ax.sock");
    let handle = Daemon::new(socket.clone()).bind().await.unwrap();

    let mut alice = connect(&socket).await;
    register(&mut alice, "alice", "/tmp/a").await;

    let env = Envelope::new(
        "req-status",
        MessageType::SetStatus,
        &SetStatusPayload {
            status: "working on migration".into(),
        },
    )
    .unwrap();
    send_envelope(&mut alice.writer, &env).await;
    let _: StatusResponse = decode_response(&await_response(&mut alice.reader, "req-status").await);

    let env = Envelope::new(
        "req-list",
        MessageType::ListWorkspaces,
        &serde_json::json!({}),
    )
    .unwrap();
    send_envelope(&mut alice.writer, &env).await;
    let list: ListWorkspacesResponse =
        decode_response(&await_response(&mut alice.reader, "req-list").await);
    let alice_info = list
        .workspaces
        .iter()
        .find(|w| w.name == "alice")
        .expect("alice registered");
    assert_eq!(alice_info.status_text, "working on migration");

    handle.shutdown().await;
}

#[tokio::test]
async fn send_to_self_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("ax.sock");
    let handle = Daemon::new(socket.clone()).bind().await.unwrap();

    let mut alice = connect(&socket).await;
    register(&mut alice, "alice", "/tmp/a").await;

    let env = Envelope::new(
        "req-self",
        MessageType::SendMessage,
        &SendMessagePayload {
            to: "alice".into(),
            message: "hi me".into(),
            config_path: String::new(),
        },
    )
    .unwrap();
    send_envelope(&mut alice.writer, &env).await;
    let resp = await_response(&mut alice.reader, "req-self").await;
    assert_eq!(resp.r#type, MessageType::Error);

    handle.shutdown().await;
}
