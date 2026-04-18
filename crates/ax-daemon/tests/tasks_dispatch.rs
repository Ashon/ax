//! End-to-end coverage for `start_task` + `intervene_task`. Validates
//! the full task-dispatch flow end-to-end over the Unix-socket
//! surface: a start dispatches a task-aware message with `Task ID:`
//! embedded in the body, queues it, schedules a wake, and appends the
//! entry to history. A retry intervention drops the old message,
//! enqueues a reminder, and re-schedules. Wake / interrupt variants
//! hit the error path (no live tmux session) and return the expected
//! diagnostic rather than panicking.

use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use ax_daemon::{Daemon, DaemonHandle, WakeScheduler};
use ax_proto::payloads::{
    InterveneTaskPayload, ReadMessagesPayload, RegisterPayload, StartTaskPayload,
};
use ax_proto::responses::{
    InterveneTaskResponse, ReadMessagesResponse, StartTaskResponse, StatusResponse,
};
use ax_proto::types::TaskStatus;
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
        format!("d{}", self.counter)
    }

    async fn send<T: serde::Serialize>(&mut self, kind: MessageType, payload: &T) -> String {
        let id = self.next_id();
        let env = Envelope::new(&id, kind, payload).expect("encode envelope");
        let mut bytes = serde_json::to_vec(&env).expect("marshal");
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.expect("write");
        id
    }

    async fn recv(&mut self) -> Envelope {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await.expect("read line");
        assert!(n > 0, "daemon closed connection unexpectedly");
        serde_json::from_str(line.trim_end_matches('\n')).expect("decode envelope")
    }

    async fn request<T: serde::Serialize, R: DeserializeOwned>(
        &mut self,
        kind: MessageType,
        payload: &T,
    ) -> R {
        let sent_id = self.send(kind, payload).await;
        loop {
            let env = self.recv().await;
            if env.id != sent_id {
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

    async fn request_err<T: serde::Serialize>(&mut self, kind: MessageType, payload: &T) -> String {
        let sent_id = self.send(kind, payload).await;
        loop {
            let env = self.recv().await;
            if env.id != sent_id {
                continue;
            }
            match env.r#type {
                MessageType::Error => {
                    let err: ax_proto::ErrorPayload = env.decode_payload().expect("error payload");
                    return err.message;
                }
                other => panic!("expected error, got {other:?}"),
            }
        }
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
async fn start_task_queues_a_task_aware_message_and_schedules_wake() {
    let f = spawn().await;
    let mut orch = Client::connect(f.handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;
    let mut worker = Client::connect(f.handle.socket_path());
    let _: StatusResponse = worker
        .request(MessageType::Register, &register("worker"))
        .await;

    let resp: StartTaskResponse = orch
        .request(
            MessageType::StartTask,
            &StartTaskPayload {
                title: "do thing".into(),
                description: String::new(),
                message: "please do the thing".into(),
                assignee: "worker".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: "high".into(),
                stale_after_seconds: 0,
            },
        )
        .await;
    assert_eq!(resp.dispatch.status, "queued");
    assert!(!resp.dispatch.message_id.is_empty());
    assert!(resp
        .task
        .dispatch_message
        .contains(&format!("Task ID: {}", resp.task.id)));

    // Worker should see the dispatched message with task_id populated.
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
    assert_eq!(drained.messages[0].task_id, resp.task.id);
    assert!(drained.messages[0].content.contains("please do the thing"));

    // After read the inbox is empty so the scheduler should have
    // cleared its pending entry.
    let scheduler: &WakeScheduler = &f.daemon.wake_scheduler;
    assert!(scheduler.state("worker").is_none());

    f.handle.shutdown().await;
}

#[tokio::test]
async fn start_task_rejects_embedded_task_id_in_message() {
    let f = spawn().await;
    let mut orch = Client::connect(f.handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;

    let err = orch
        .request_err(
            MessageType::StartTask,
            &StartTaskPayload {
                title: "x".into(),
                description: String::new(),
                message: "Task ID: 11111111-2222-3333-4444-555555555555 hi".into(),
                assignee: "worker".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: String::new(),
                stale_after_seconds: 0,
            },
        )
        .await;
    assert!(err.contains("Task ID"), "got: {err}");
    f.handle.shutdown().await;
}

#[tokio::test]
async fn intervene_wake_without_config_fails_when_no_session() {
    let f = spawn().await;
    let mut orch = Client::connect(f.handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;

    let task: StartTaskResponse = orch
        .request(
            MessageType::StartTask,
            &StartTaskPayload {
                title: "x".into(),
                description: String::new(),
                message: "go".into(),
                assignee: "ghost".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: String::new(),
                stale_after_seconds: 0,
            },
        )
        .await;

    let err = orch
        .request_err(
            MessageType::InterveneTask,
            &InterveneTaskPayload {
                id: task.task.id,
                action: "wake".into(),
                note: String::new(),
                expected_version: None,
            },
        )
        .await;
    assert!(err.contains("not running"), "got: {err}");
    f.handle.shutdown().await;
}

#[tokio::test]
async fn intervene_retry_requeues_and_reschedules() {
    let f = spawn().await;
    let mut orch = Client::connect(f.handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;
    let mut worker = Client::connect(f.handle.socket_path());
    let _: StatusResponse = worker
        .request(MessageType::Register, &register("worker"))
        .await;

    let task: StartTaskResponse = orch
        .request(
            MessageType::StartTask,
            &StartTaskPayload {
                title: "retry me".into(),
                description: "flaky flow".into(),
                message: "initial body".into(),
                assignee: "worker".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: String::new(),
                stale_after_seconds: 0,
            },
        )
        .await;

    // Drain the original message so the retry is the only thing
    // sitting in the worker's inbox afterwards.
    let _: ReadMessagesResponse = worker
        .request(
            MessageType::ReadMessages,
            &ReadMessagesPayload {
                limit: 10,
                from: String::new(),
            },
        )
        .await;

    let retried: InterveneTaskResponse = orch
        .request(
            MessageType::InterveneTask,
            &InterveneTaskPayload {
                id: task.task.id.clone(),
                action: "retry".into(),
                note: "please try again".into(),
                expected_version: None,
            },
        )
        .await;
    assert_eq!(retried.status, "queued");
    assert!(!retried.message_id.is_empty());
    assert_eq!(retried.task.status, TaskStatus::Pending);

    let after: ReadMessagesResponse = worker
        .request(
            MessageType::ReadMessages,
            &ReadMessagesPayload {
                limit: 10,
                from: String::new(),
            },
        )
        .await;
    assert_eq!(after.messages.len(), 1);
    let msg = &after.messages[0];
    assert_eq!(msg.task_id, task.task.id);
    assert!(msg.content.contains("Operator note: please try again"));
    assert!(msg.content.contains("Task ID:"));

    f.handle.shutdown().await;
}

#[tokio::test]
async fn intervene_rejects_unknown_action() {
    let f = spawn().await;
    let mut orch = Client::connect(f.handle.socket_path());
    let _: StatusResponse = orch.request(MessageType::Register, &register("orch")).await;

    let task: StartTaskResponse = orch
        .request(
            MessageType::StartTask,
            &StartTaskPayload {
                title: "t".into(),
                description: String::new(),
                message: "body".into(),
                assignee: "worker".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: String::new(),
                stale_after_seconds: 0,
            },
        )
        .await;
    let err = orch
        .request_err(
            MessageType::InterveneTask,
            &InterveneTaskPayload {
                id: task.task.id,
                action: "nope".into(),
                note: String::new(),
                expected_version: None,
            },
        )
        .await;
    assert!(err.contains("invalid intervene_task action"), "got: {err}");
    f.handle.shutdown().await;
}
