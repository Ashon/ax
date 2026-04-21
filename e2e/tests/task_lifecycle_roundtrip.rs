//! Wire-level coverage for the task-lifecycle improvements added
//! across the stale-lifecycle autoresearch run. The MCP-server
//! integration tests already exercise the typed tool methods against
//! an in-process daemon; this file drives the same behaviours through
//! the Unix-socket envelope protocol to catch regressions in the
//! JSON-RPC wire shape that single-crate tests miss.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ax_daemon::{Daemon, DaemonHandle};
use ax_proto::payloads::{
    CreateTaskPayload, ReadMessagesPayload, RegisterPayload, UpdateTaskPayload,
};
use ax_proto::responses::{ReadMessagesResponse, StatusResponse, TaskResponse};
use ax_proto::types::TaskStatus;
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
async fn completed_task_pushes_notification_into_creator_inbox() {
    // Goal: validate the "terminal-status push" added in the
    // stale-lifecycle run over the real daemon socket. An
    // orchestrator (alice) creates a task for a worker (bob).
    // Bob marks it completed with the proper marker. Alice's
    // inbox should receive a `[task-completed]` system message
    // the next time it polls — no out-of-band query needed.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    let inbox = tokio::task::spawn_blocking(move || {
        let mut alice = SyncClient::connect(&socket, "alice").expect("alice");
        let mut bob = SyncClient::connect(&socket, "bob").expect("bob");

        let created: TaskResponse = alice
            .request(
                MessageType::CreateTask,
                &CreateTaskPayload {
                    title: "ship the thing".into(),
                    assignee: "bob".into(),
                    ..Default::default()
                },
            )
            .expect("create");
        let task_id = created.task.id.clone();

        // Bob transitions to in_progress first (InProgress → Completed
        // is the allowed path; Pending → Completed is also legal but
        // this mirrors how a real worker would use the heartbeat).
        let _: TaskResponse = bob
            .request(
                MessageType::UpdateTask,
                &UpdateTaskPayload {
                    id: task_id.clone(),
                    status: Some(TaskStatus::InProgress),
                    log: Some("starting".into()),
                    ..Default::default()
                },
            )
            .expect("promote");

        // Close the loop with a compliant marker + confirm.
        let _: TaskResponse = bob
            .request(
                MessageType::UpdateTask,
                &UpdateTaskPayload {
                    id: task_id.clone(),
                    status: Some(TaskStatus::Completed),
                    result: Some(
                        "shipped; remaining owned dirty files=<none>".into(),
                    ),
                    confirm: Some(true),
                    ..Default::default()
                },
            )
            .expect("complete");

        let inbox: ReadMessagesResponse = alice
            .request(
                MessageType::ReadMessages,
                &ReadMessagesPayload {
                    limit: 10,
                    from: String::new(),
                },
            )
            .expect("read");
        (inbox, task_id)
    })
    .await
    .expect("blocking");

    let (inbox, task_id) = inbox;
    assert_eq!(inbox.messages.len(), 1, "exactly one push expected");
    let msg = &inbox.messages[0];
    assert_eq!(msg.from, "bob");
    assert_eq!(msg.to, "alice");
    assert!(
        msg.content.contains("[task-completed]"),
        "content: {}",
        msg.content
    );
    assert!(
        msg.content.contains(&task_id),
        "content must carry task id: {}",
        msg.content
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn failed_task_pushes_notification_with_reason_to_creator() {
    // The terminal-status push covers every terminal state, not
    // just Completed. Verify Failed goes through the same channel
    // so orchestrators see hard failures without polling get_task.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    let inbox = tokio::task::spawn_blocking(move || {
        let mut alice = SyncClient::connect(&socket, "alice").expect("alice");
        let mut bob = SyncClient::connect(&socket, "bob").expect("bob");

        let created: TaskResponse = alice
            .request(
                MessageType::CreateTask,
                &CreateTaskPayload {
                    title: "blocked thing".into(),
                    assignee: "bob".into(),
                    ..Default::default()
                },
            )
            .expect("create");
        let _: TaskResponse = bob
            .request(
                MessageType::UpdateTask,
                &UpdateTaskPayload {
                    id: created.task.id.clone(),
                    status: Some(TaskStatus::Failed),
                    result: Some("failed: upstream 503 persisted".into()),
                    ..Default::default()
                },
            )
            .expect("fail");

        let inbox: ReadMessagesResponse = alice
            .request(
                MessageType::ReadMessages,
                &ReadMessagesPayload {
                    limit: 10,
                    from: String::new(),
                },
            )
            .expect("read");
        inbox
    })
    .await
    .expect("blocking");

    assert_eq!(inbox.messages.len(), 1);
    let msg = &inbox.messages[0];
    assert!(
        msg.content.contains("[task-failed]"),
        "content: {}",
        msg.content
    );
    assert!(msg.content.contains("503"), "content: {}", msg.content);

    handle.shutdown().await;
}

#[tokio::test]
async fn completion_without_marker_enqueues_reminder_into_worker_inbox() {
    // When a worker tries to close a task without the leftover-scope
    // marker, the daemon returns a descriptive error AND pushes a
    // durable reminder into the worker's own inbox so the next
    // `read_messages` resurfaces the contract requirement.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    let (err_text, reminder) = tokio::task::spawn_blocking(move || {
        let mut alice = SyncClient::connect(&socket, "alice").expect("alice");
        let mut bob = SyncClient::connect(&socket, "bob").expect("bob");

        let created: TaskResponse = alice
            .request(
                MessageType::CreateTask,
                &CreateTaskPayload {
                    title: "t".into(),
                    assignee: "bob".into(),
                    ..Default::default()
                },
            )
            .expect("create");

        // Missing marker AND confirm — the daemon rejects and
        // enqueues the reminder as a side effect.
        let err: Result<TaskResponse, _> = bob.request(
            MessageType::UpdateTask,
            &UpdateTaskPayload {
                id: created.task.id.clone(),
                status: Some(TaskStatus::Completed),
                result: Some("looks good".into()),
                confirm: Some(true),
                ..Default::default()
            },
        );
        let err_text = err.expect_err("completion must be rejected").to_string();

        let inbox: ReadMessagesResponse = bob
            .request(
                MessageType::ReadMessages,
                &ReadMessagesPayload {
                    limit: 10,
                    from: String::new(),
                },
            )
            .expect("read");
        (err_text, inbox)
    })
    .await
    .expect("blocking");

    assert!(
        err_text.contains("leftover-scope"),
        "rejection should mention the contract: {err_text}"
    );
    assert_eq!(reminder.messages.len(), 1, "exactly one reminder expected");
    let msg = &reminder.messages[0];
    assert_eq!(msg.to, "bob");
    assert!(
        msg.content.contains("[task-completion-rejected]"),
        "content: {}",
        msg.content
    );
    assert!(
        msg.content.contains("remaining owned dirty files"),
        "content should echo the remediation: {}",
        msg.content
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn self_assigned_task_completion_does_not_push_creator_notification() {
    // When creator == assignee (single agent working on its own
    // backlog) the terminal-status push would be self-noise. Verify
    // the daemon suppresses it at wire level, same as the in-process
    // test proves at MCP level.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    let inbox = tokio::task::spawn_blocking(move || {
        let mut alice = SyncClient::connect(&socket, "alice").expect("alice");
        let created: TaskResponse = alice
            .request(
                MessageType::CreateTask,
                &CreateTaskPayload {
                    title: "self".into(),
                    assignee: "alice".into(),
                    ..Default::default()
                },
            )
            .expect("create");
        let _: TaskResponse = alice
            .request(
                MessageType::UpdateTask,
                &UpdateTaskPayload {
                    id: created.task.id.clone(),
                    status: Some(TaskStatus::Completed),
                    result: Some("done; remaining owned dirty files=<none>".into()),
                    confirm: Some(true),
                    ..Default::default()
                },
            )
            .expect("complete");
        let inbox: ReadMessagesResponse = alice
            .request(
                MessageType::ReadMessages,
                &ReadMessagesPayload {
                    limit: 10,
                    from: String::new(),
                },
            )
            .expect("read");
        inbox
    })
    .await
    .expect("blocking");

    assert!(
        inbox.messages.is_empty(),
        "self-assigned task must not notify self: {:?}",
        inbox.messages
    );

    handle.shutdown().await;
}
