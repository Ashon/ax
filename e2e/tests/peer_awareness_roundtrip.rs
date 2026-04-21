//! Wire-level coverage for the peer-awareness fields added to
//! `WorkspaceInfo` during the peer-awareness autoresearch run. The
//! in-process MCP-server tests verify the Rust-struct plumbing; this
//! file replays the same observations through the Unix-socket
//! `ListWorkspaces` roundtrip so regressions in serde shape, default
//! handling, or registry → response mapping surface here too.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ax_daemon::{Daemon, DaemonHandle};
use ax_proto::payloads::{CreateTaskPayload, RegisterPayload, UpdateTaskPayload};
use ax_proto::responses::{ListWorkspacesResponse, StatusResponse, TaskResponse};
use ax_proto::types::TaskStatus;
use ax_proto::{Envelope, ErrorPayload, MessageType, ResponsePayload};
use serde::de::DeserializeOwned;
use tempfile::TempDir;

struct SyncClient {
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl SyncClient {
    fn connect_with_idle(
        socket: &std::path::Path,
        workspace: &str,
        idle_timeout_seconds: i64,
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
                idle_timeout_seconds,
            },
        )?;
        Ok(client)
    }

    fn connect(
        socket: &std::path::Path,
        workspace: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::connect_with_idle(socket, workspace, 0)
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
async fn list_workspaces_returns_liveness_timestamps_for_registered_peers() {
    // Validate that the fresh peer-awareness fields (`last_activity_at`,
    // `connection_generation`, `connected_at`) survive the serde
    // roundtrip and land on the client with sensible shapes.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    let resp = tokio::task::spawn_blocking(move || {
        let mut _alice = SyncClient::connect(&socket, "alice").expect("alice");
        let mut bob = SyncClient::connect(&socket, "bob").expect("bob");
        let listed: ListWorkspacesResponse = bob
            .request(
                MessageType::ListWorkspaces,
                &serde_json::json!({}),
            )
            .expect("list");
        listed
    })
    .await
    .expect("blocking");

    assert_eq!(resp.workspaces.len(), 2);
    for ws in &resp.workspaces {
        assert!(
            ws.last_activity_at.is_some(),
            "{} must report last_activity_at",
            ws.name
        );
        assert!(
            ws.connected_at.is_some(),
            "{} must report connected_at",
            ws.name
        );
        assert!(
            ws.connection_generation > 0,
            "{} must have a nonzero connection_generation",
            ws.name
        );
    }
    // The two peers should have distinct generations — fresh
    // registrations are monotonically numbered.
    let mut gens: Vec<u64> = resp
        .workspaces
        .iter()
        .map(|w| w.connection_generation)
        .collect();
    gens.sort_unstable();
    assert_ne!(gens[0], gens[1], "peer generations must differ");

    handle.shutdown().await;
}

#[tokio::test]
async fn list_workspaces_reports_active_task_count_and_current_task_id() {
    // Create two tasks for bob (one still pending, one promoted to
    // in_progress). `active_task_count` counts both; `current_task_id`
    // only surfaces the in-progress one.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    let (listed, running_id) = tokio::task::spawn_blocking(move || {
        let mut alice = SyncClient::connect(&socket, "alice").expect("alice");
        let mut bob = SyncClient::connect(&socket, "bob").expect("bob");

        let _pending: TaskResponse = alice
            .request(
                MessageType::CreateTask,
                &CreateTaskPayload {
                    title: "waiting".into(),
                    assignee: "bob".into(),
                    ..Default::default()
                },
            )
            .expect("create pending");
        let running: TaskResponse = alice
            .request(
                MessageType::CreateTask,
                &CreateTaskPayload {
                    title: "running".into(),
                    assignee: "bob".into(),
                    ..Default::default()
                },
            )
            .expect("create running");
        let running_id = running.task.id.clone();
        let _: TaskResponse = bob
            .request(
                MessageType::UpdateTask,
                &UpdateTaskPayload {
                    id: running_id.clone(),
                    status: Some(TaskStatus::InProgress),
                    log: Some("started".into()),
                    ..Default::default()
                },
            )
            .expect("promote");

        let listed: ListWorkspacesResponse = alice
            .request(
                MessageType::ListWorkspaces,
                &serde_json::json!({}),
            )
            .expect("list");
        (listed, running_id)
    })
    .await
    .expect("blocking");

    let bob_entry = listed
        .workspaces
        .iter()
        .find(|w| w.name == "bob")
        .expect("bob must appear");
    assert_eq!(
        bob_entry.active_task_count, 2,
        "pending + in_progress should both count: {bob_entry:?}"
    );
    assert_eq!(
        bob_entry.current_task_id.as_deref(),
        Some(running_id.as_str()),
        "current_task_id should point at the in_progress task: {bob_entry:?}"
    );

    // Alice has no owned tasks → both fields should be empty.
    let alice_entry = listed
        .workspaces
        .iter()
        .find(|w| w.name == "alice")
        .expect("alice must appear");
    assert_eq!(alice_entry.active_task_count, 0);
    assert!(alice_entry.current_task_id.is_none());

    handle.shutdown().await;
}

#[tokio::test]
async fn list_workspaces_carries_declared_idle_timeout_through_the_wire() {
    // The idle_timeout a peer declares at Register time must survive
    // the serde round-trip so orchestrators can compare it against
    // last_activity_at for proactive intervention.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    let listed = tokio::task::spawn_blocking(move || {
        let _bob = SyncClient::connect_with_idle(&socket, "bob", 90).expect("bob");
        let mut alice = SyncClient::connect(&socket, "alice").expect("alice");
        let listed: ListWorkspacesResponse = alice
            .request(
                MessageType::ListWorkspaces,
                &serde_json::json!({}),
            )
            .expect("list");
        listed
    })
    .await
    .expect("blocking");

    let bob_entry = listed
        .workspaces
        .iter()
        .find(|w| w.name == "bob")
        .expect("bob must appear");
    assert_eq!(bob_entry.idle_timeout_seconds, 90, "entry: {bob_entry:?}");

    let alice_entry = listed
        .workspaces
        .iter()
        .find(|w| w.name == "alice")
        .expect("alice must appear");
    assert_eq!(
        alice_entry.idle_timeout_seconds, 0,
        "default-registered peer should surface 0: {alice_entry:?}"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn connection_generation_bumps_on_reregister_over_the_wire() {
    // Disconnecting and re-registering the same workspace name must
    // hand out a strictly greater generation so callers can invalidate
    // any cached state tied to the previous connection.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    let (before, after) = tokio::task::spawn_blocking(move || {
        let alice1 = SyncClient::connect(&socket, "alice").expect("alice-1");
        let mut observer = SyncClient::connect(&socket, "observer").expect("obs");
        let listed_before: ListWorkspacesResponse = observer
            .request(
                MessageType::ListWorkspaces,
                &serde_json::json!({}),
            )
            .expect("list-1");
        let before = listed_before
            .workspaces
            .iter()
            .find(|w| w.name == "alice")
            .map(|w| w.connection_generation)
            .expect("alice in snapshot 1");

        // Drop alice's socket by dropping the client; give the
        // daemon a moment to notice, then re-register.
        drop(alice1);
        std::thread::sleep(Duration::from_millis(100));
        let _alice2 = SyncClient::connect(&socket, "alice").expect("alice-2");

        let listed_after: ListWorkspacesResponse = observer
            .request(
                MessageType::ListWorkspaces,
                &serde_json::json!({}),
            )
            .expect("list-2");
        let after = listed_after
            .workspaces
            .iter()
            .find(|w| w.name == "alice")
            .map(|w| w.connection_generation)
            .expect("alice in snapshot 2");

        (before, after)
    })
    .await
    .expect("blocking");

    assert!(
        after > before,
        "re-register must bump generation: before={before}, after={after}"
    );

    handle.shutdown().await;
}
