//! `set_shared` / `get_shared` / `list_shared` end-to-end: in-memory
//! round-trip plus persistence survival across a restart.

use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use ax_daemon::Daemon;
use ax_proto::payloads::{GetSharedPayload, SetSharedPayload};
use ax_proto::responses::{GetSharedResponse, ListSharedResponse, StatusResponse};
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

#[tokio::test]
async fn in_memory_shared_values_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("ax.sock");
    let handle = Daemon::new(socket.clone()).bind().await.unwrap();

    let mut client = connect(&socket).await;

    // set_shared
    let env = Envelope::new(
        "req-set",
        MessageType::SetShared,
        &SetSharedPayload {
            key: "release_gate".into(),
            value: "open".into(),
        },
    )
    .unwrap();
    send_envelope(&mut client.writer, &env).await;
    let resp: StatusResponse =
        decode_response(&await_response(&mut client.reader, "req-set").await);
    assert_eq!(resp.status, "stored");

    // get_shared: present
    let env = Envelope::new(
        "req-get",
        MessageType::GetShared,
        &GetSharedPayload {
            key: "release_gate".into(),
        },
    )
    .unwrap();
    send_envelope(&mut client.writer, &env).await;
    let resp: GetSharedResponse =
        decode_response(&await_response(&mut client.reader, "req-get").await);
    assert!(resp.found);
    assert_eq!(resp.value, "open");

    // get_shared: missing
    let env = Envelope::new(
        "req-miss",
        MessageType::GetShared,
        &GetSharedPayload {
            key: "absent".into(),
        },
    )
    .unwrap();
    send_envelope(&mut client.writer, &env).await;
    let resp: GetSharedResponse =
        decode_response(&await_response(&mut client.reader, "req-miss").await);
    assert!(!resp.found);
    assert_eq!(resp.value, "");

    // list_shared
    let env = Envelope::new("req-list", MessageType::ListShared, &serde_json::json!({})).unwrap();
    send_envelope(&mut client.writer, &env).await;
    let resp: ListSharedResponse =
        decode_response(&await_response(&mut client.reader, "req-list").await);
    assert_eq!(resp.values.len(), 1);
    assert_eq!(
        resp.values.get("release_gate").map(String::as_str),
        Some("open")
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn shared_values_survive_restart_via_state_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_a = tmp.path().join("a.sock");
    let state = tmp.path().join("state");

    // Session 1: write a key and shut down.
    {
        let daemon = Daemon::new(socket_a.clone())
            .with_state_dir(&state)
            .expect("load empty state");
        let handle = daemon.bind().await.unwrap();
        let mut client = connect(&socket_a).await;
        let env = Envelope::new(
            "req-set",
            MessageType::SetShared,
            &SetSharedPayload {
                key: "k".into(),
                value: "v".into(),
            },
        )
        .unwrap();
        send_envelope(&mut client.writer, &env).await;
        let _: StatusResponse =
            decode_response(&await_response(&mut client.reader, "req-set").await);
        handle.shutdown().await;
    }

    // Session 2: a brand-new daemon rehydrates from the same state dir.
    let socket_b = tmp.path().join("b.sock");
    let daemon = Daemon::new(socket_b.clone())
        .with_state_dir(&state)
        .expect("load persisted state");
    let handle = daemon.bind().await.unwrap();
    let mut client = connect(&socket_b).await;
    let env = Envelope::new(
        "req-get",
        MessageType::GetShared,
        &GetSharedPayload { key: "k".into() },
    )
    .unwrap();
    send_envelope(&mut client.writer, &env).await;
    let resp: GetSharedResponse =
        decode_response(&await_response(&mut client.reader, "req-get").await);
    assert!(resp.found);
    assert_eq!(resp.value, "v");

    handle.shutdown().await;
}
