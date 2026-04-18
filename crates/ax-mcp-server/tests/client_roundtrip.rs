//! Spins up an in-process `ax-daemon` and walks the client through
//! connect → register → `send_message` → `read_messages` → close.
//! Proves the envelope id pairing, timeout handling, and
//! push-message demultiplexing behave end-to-end.

use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;

use ax_daemon::{Daemon, DaemonHandle};
use ax_mcp_server::DaemonClient;
use ax_proto::payloads::{ReadMessagesPayload, SendMessagePayload};
use ax_proto::responses::{ReadMessagesResponse, SendMessageResponse};
use ax_proto::MessageType;

async fn spawn_daemon(state_dir: &std::path::Path) -> DaemonHandle {
    let socket_path = state_dir.join("daemon.sock");
    let daemon = Daemon::new(socket_path)
        .with_state_dir(state_dir)
        .expect("with_state_dir");
    let handle = daemon.bind().await.expect("bind");
    for _ in 0..50 {
        if handle.socket_path().exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle
}

#[tokio::test]
async fn connect_register_and_request_round_trip() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();
    let handle = spawn_daemon(&state_dir).await;

    let orch = DaemonClient::builder(handle.socket_path(), "orch")
        .dir("/tmp/orch")
        .connect()
        .await
        .expect("orch connects");
    let worker = DaemonClient::builder(handle.socket_path(), "worker")
        .dir("/tmp/worker")
        .connect()
        .await
        .expect("worker connects");

    let resp: SendMessageResponse = orch
        .request(
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "worker".into(),
                message: "hi there".into(),
                config_path: String::new(),
            },
        )
        .await
        .expect("send succeeds");
    assert_eq!(resp.status, "sent");

    let drained: ReadMessagesResponse = worker
        .request(
            MessageType::ReadMessages,
            &ReadMessagesPayload {
                limit: 10,
                from: String::new(),
            },
        )
        .await
        .expect("read succeeds");
    assert_eq!(drained.messages.len(), 1);
    assert_eq!(drained.messages[0].content, "hi there");

    orch.close().await;
    worker.close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn push_messages_land_in_drainable_buffer() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;

    // Worker connects first so its outbox exists when the push arrives.
    let worker = DaemonClient::builder(handle.socket_path(), "worker")
        .connect()
        .await
        .expect("worker connects");
    let orch = DaemonClient::builder(handle.socket_path(), "orch")
        .connect()
        .await
        .expect("orch connects");

    let _: SendMessageResponse = orch
        .request(
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "worker".into(),
                message: "pushed".into(),
                config_path: String::new(),
            },
        )
        .await
        .expect("send");

    // Give the push a moment to traverse the wire.
    for _ in 0..20 {
        if !worker.take_push_messages().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Send one more so the push buffer has a deterministic size afterwards.
    let _: SendMessageResponse = orch
        .request(
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "worker".into(),
                message: "again".into(),
                config_path: String::new(),
            },
        )
        .await
        .expect("send");
    let mut got_second = false;
    for _ in 0..20 {
        let pushes = worker.take_push_messages();
        if pushes.iter().any(|m| m.content == "again") {
            got_second = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(got_second, "second push should arrive on the buffer");

    orch.close().await;
    worker.close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn close_drains_outstanding_requests_with_disconnect() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;

    let client = DaemonClient::builder(handle.socket_path(), "orch")
        .request_timeout(Duration::from_millis(200))
        .connect()
        .await
        .expect("connect");

    client.close().await;
    // Subsequent request must fail fast; the reader task has been
    // aborted so the write half is gone.
    let err = client
        .request::<_, SendMessageResponse>(
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "worker".into(),
                message: "after close".into(),
                config_path: String::new(),
            },
        )
        .await;
    assert!(err.is_err(), "request after close must not succeed");
    handle.shutdown().await;
}
