//! Cross-crate smoke test: boot an in-process `ax-daemon`, connect a
//! sync Unix-socket client, register two workspaces, exchange a
//! message, and verify the recipient's inbox. This is the same
//! shape the CLI + MCP server drive in production, so a regression
//! here catches wire-level breakage that single-crate unit tests
//! miss.
//!
//! Tests in this crate are gated by the standard `cargo test` flow
//! (no `#[ignore]`) because they don't touch a real tmux server or
//! the network — the daemon binds a tempfile socket and every
//! dependency is pulled in-process.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ax_daemon::{Daemon, DaemonHandle};
use ax_proto::payloads::{ReadMessagesPayload, RegisterPayload, SendMessagePayload};
use ax_proto::responses::{ReadMessagesResponse, SendMessageResponse, StatusResponse};
use ax_proto::{Envelope, ErrorPayload, MessageType, ResponsePayload};
use serde::de::DeserializeOwned;
use tempfile::TempDir;

struct SyncClient {
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl SyncClient {
    fn connect(
        socket: &std::path::Path,
        workspace: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut client = Self {
            reader: BufReader::new(stream),
            next_id: 1,
        };
        let _: StatusResponse = client.request(
            MessageType::Register,
            &RegisterPayload {
                workspace: workspace.to_owned(),
                dir: "/tmp".into(),
                description: String::new(),
                config_path: String::new(),
                idle_timeout_seconds: 0,
            },
        )?;
        Ok(client)
    }

    fn request<P, R>(
        &mut self,
        kind: MessageType,
        payload: &P,
    ) -> Result<R, Box<dyn std::error::Error>>
    where
        P: serde::Serialize,
        R: DeserializeOwned,
    {
        let id = format!("e2e-{}", self.next_id);
        self.next_id += 1;
        let env = Envelope::new(&id, kind, payload)?;
        let mut bytes = serde_json::to_vec(&env)?;
        bytes.push(b'\n');
        self.reader.get_mut().write_all(&bytes)?;
        self.reader.get_mut().flush()?;
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line)?;
            if read == 0 {
                return Err("connection closed".into());
            }
            let env: Envelope = serde_json::from_str(line.trim_end())?;
            if env.id != id {
                continue;
            }
            match env.r#type {
                MessageType::Response => {
                    let wrap: ResponsePayload = env.decode_payload()?;
                    return Ok(serde_json::from_str(wrap.data.get())?);
                }
                MessageType::Error => {
                    let err: ErrorPayload = env.decode_payload()?;
                    return Err(err.message.into());
                }
                other => return Err(format!("unexpected envelope {other:?}").into()),
            }
        }
    }
}

async fn spawn_daemon(state_dir: &std::path::Path) -> DaemonHandle {
    let socket = state_dir.join("daemon.sock");
    let handle = Daemon::new(socket)
        .with_state_dir(state_dir)
        .expect("state_dir accepted")
        .bind()
        .await
        .expect("daemon binds");
    for _ in 0..50 {
        if handle.socket_path().exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle
}

#[tokio::test]
async fn daemon_roundtrip_send_message_lands_in_recipient_inbox() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;

    let socket = handle.socket_path().to_path_buf();
    let (recv, sent) = tokio::task::spawn_blocking(move || {
        let mut alice = SyncClient::connect(&socket, "alice").expect("alice");
        let mut bob = SyncClient::connect(&socket, "bob").expect("bob");

        let sent: SendMessageResponse = alice
            .request(
                MessageType::SendMessage,
                &SendMessagePayload {
                    to: "bob".into(),
                    message: "hello from alice".into(),
                    config_path: String::new(),
                },
            )
            .expect("send");

        let inbox: ReadMessagesResponse = bob
            .request(
                MessageType::ReadMessages,
                &ReadMessagesPayload {
                    limit: 10,
                    from: String::new(),
                },
            )
            .expect("read");
        (inbox, sent)
    })
    .await
    .expect("join");

    assert!(!sent.message_id.is_empty(), "expected a message id");
    assert_eq!(recv.messages.len(), 1);
    let msg = &recv.messages[0];
    assert_eq!(msg.from, "alice");
    assert_eq!(msg.to, "bob");
    assert_eq!(msg.content, "hello from alice");

    handle.shutdown().await;
}
