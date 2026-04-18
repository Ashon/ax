//! End-to-end coverage for the task-store handlers landed in slice 1:
//! create / get / list / update / cancel / remove. Validates the
//! round-trip shape of each response envelope, the rollup refresh
//! behaviour on children changes, and the persist / reload cycle.

use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use ax_daemon::{Daemon, DaemonHandle};
use ax_proto::payloads::{
    CancelTaskPayload, CreateTaskPayload, GetTaskPayload, ListTasksPayload, RegisterPayload,
    RemoveTaskPayload, UpdateTaskPayload,
};
use ax_proto::responses::{ListTasksResponse, StatusResponse, TaskResponse};
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
        format!("t{}", self.counter)
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

fn register_payload(workspace: &str) -> RegisterPayload {
    RegisterPayload {
        workspace: workspace.to_owned(),
        dir: format!("/tmp/{workspace}"),
        description: String::new(),
        config_path: String::new(),
        idle_timeout_seconds: 0,
    }
}

async fn wait_for_socket(socket: &Path) {
    for _ in 0..50 {
        if socket.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn create_get_list_update_cycle() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    wait_for_socket(handle.socket_path()).await;

    let mut orch = Client::connect(handle.socket_path());
    let _: StatusResponse = orch
        .request(MessageType::Register, &register_payload("orch"))
        .await;

    let mut worker = Client::connect(handle.socket_path());
    let _: StatusResponse = worker
        .request(MessageType::Register, &register_payload("worker"))
        .await;

    let created: TaskResponse = orch
        .request(
            MessageType::CreateTask,
            &CreateTaskPayload {
                title: "fix bug".into(),
                description: "see #42".into(),
                assignee: "worker".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: "high".into(),
                stale_after_seconds: 0,
            },
        )
        .await;
    assert_eq!(created.task.title, "fix bug");
    assert_eq!(created.task.assignee, "worker");
    assert_eq!(created.task.created_by, "orch");
    assert_eq!(created.task.status, TaskStatus::Pending);
    assert_eq!(created.task.version, 1);

    let fetched: TaskResponse = orch
        .request(
            MessageType::GetTask,
            &GetTaskPayload {
                id: created.task.id.clone(),
            },
        )
        .await;
    assert_eq!(fetched.task.id, created.task.id);

    let listed: ListTasksResponse = orch
        .request(
            MessageType::ListTasks,
            &ListTasksPayload {
                assignee: "worker".into(),
                created_by: String::new(),
                status: None,
            },
        )
        .await;
    assert_eq!(listed.tasks.len(), 1);

    let updated: TaskResponse = worker
        .request(
            MessageType::UpdateTask,
            &UpdateTaskPayload {
                id: created.task.id.clone(),
                status: Some(TaskStatus::InProgress),
                result: None,
                log: Some("starting work".into()),
            },
        )
        .await;
    assert_eq!(updated.task.status, TaskStatus::InProgress);
    assert_eq!(updated.task.logs.len(), 1);
    assert!(updated.task.claimed_at.is_some());
    assert_eq!(updated.task.claimed_by, "worker");

    handle.shutdown().await;
}

#[tokio::test]
async fn update_enforces_permissions_and_transitions() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    wait_for_socket(handle.socket_path()).await;

    let mut orch = Client::connect(handle.socket_path());
    let _: StatusResponse = orch
        .request(MessageType::Register, &register_payload("orch"))
        .await;
    let mut worker = Client::connect(handle.socket_path());
    let _: StatusResponse = worker
        .request(MessageType::Register, &register_payload("worker"))
        .await;
    let mut stranger = Client::connect(handle.socket_path());
    let _: StatusResponse = stranger
        .request(MessageType::Register, &register_payload("stranger"))
        .await;

    let created: TaskResponse = orch
        .request(
            MessageType::CreateTask,
            &CreateTaskPayload {
                title: "x".into(),
                description: String::new(),
                assignee: "worker".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: String::new(),
                stale_after_seconds: 0,
            },
        )
        .await;

    // Stranger cannot update
    let sent_id = stranger
        .send(
            MessageType::UpdateTask,
            &UpdateTaskPayload {
                id: created.task.id.clone(),
                status: Some(TaskStatus::InProgress),
                result: None,
                log: None,
            },
        )
        .await;
    let env = loop {
        let env = stranger.recv().await;
        if env.id == sent_id {
            break env;
        }
    };
    assert_eq!(env.r#type, MessageType::Error);

    // Completed then re-open must fail
    let _: TaskResponse = worker
        .request(
            MessageType::UpdateTask,
            &UpdateTaskPayload {
                id: created.task.id.clone(),
                status: Some(TaskStatus::Completed),
                result: Some("done".into()),
                log: None,
            },
        )
        .await;
    let sent_id = worker
        .send(
            MessageType::UpdateTask,
            &UpdateTaskPayload {
                id: created.task.id.clone(),
                status: Some(TaskStatus::InProgress),
                result: None,
                log: None,
            },
        )
        .await;
    let env = loop {
        let env = worker.recv().await;
        if env.id == sent_id {
            break env;
        }
    };
    assert_eq!(env.r#type, MessageType::Error);

    handle.shutdown().await;
}

#[tokio::test]
async fn parent_rollup_refreshes_on_child_completion() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    wait_for_socket(handle.socket_path()).await;

    let mut orch = Client::connect(handle.socket_path());
    let _: StatusResponse = orch
        .request(MessageType::Register, &register_payload("orch"))
        .await;

    let parent: TaskResponse = orch
        .request(
            MessageType::CreateTask,
            &CreateTaskPayload {
                title: "umbrella".into(),
                description: String::new(),
                assignee: "orch".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: String::new(),
                stale_after_seconds: 0,
            },
        )
        .await;

    let child: TaskResponse = orch
        .request(
            MessageType::CreateTask,
            &CreateTaskPayload {
                title: "leaf".into(),
                description: String::new(),
                assignee: "orch".into(),
                parent_task_id: parent.task.id.clone(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: String::new(),
                stale_after_seconds: 0,
            },
        )
        .await;

    let fetched_parent: TaskResponse = orch
        .request(
            MessageType::GetTask,
            &GetTaskPayload {
                id: parent.task.id.clone(),
            },
        )
        .await;
    let rollup = fetched_parent.task.rollup.expect("parent rollup");
    assert_eq!(rollup.total_children, 1);
    assert_eq!(rollup.pending_children, 1);
    assert!(!rollup.all_children_terminal);

    let _: TaskResponse = orch
        .request(
            MessageType::UpdateTask,
            &UpdateTaskPayload {
                id: child.task.id.clone(),
                status: Some(TaskStatus::Completed),
                result: Some("ok".into()),
                log: None,
            },
        )
        .await;

    let refreshed_parent: TaskResponse = orch
        .request(
            MessageType::GetTask,
            &GetTaskPayload {
                id: parent.task.id.clone(),
            },
        )
        .await;
    let rollup = refreshed_parent.task.rollup.expect("parent rollup");
    assert_eq!(rollup.completed_children, 1);
    assert!(rollup.all_children_terminal);
    assert!(rollup.needs_parent_reconciliation);

    handle.shutdown().await;
}

#[tokio::test]
async fn cancel_and_remove_drop_pending_task_messages() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    wait_for_socket(handle.socket_path()).await;

    let mut orch = Client::connect(handle.socket_path());
    let _: StatusResponse = orch
        .request(MessageType::Register, &register_payload("orch"))
        .await;

    let created: TaskResponse = orch
        .request(
            MessageType::CreateTask,
            &CreateTaskPayload {
                title: "nope".into(),
                description: String::new(),
                assignee: "worker".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: String::new(),
                stale_after_seconds: 0,
            },
        )
        .await;

    let cancelled: TaskResponse = orch
        .request(
            MessageType::CancelTask,
            &CancelTaskPayload {
                id: created.task.id.clone(),
                reason: "scope cut".into(),
                expected_version: None,
            },
        )
        .await;
    assert_eq!(cancelled.task.status, TaskStatus::Cancelled);

    let removed: TaskResponse = orch
        .request(
            MessageType::RemoveTask,
            &RemoveTaskPayload {
                id: created.task.id.clone(),
                reason: "cleanup".into(),
                expected_version: None,
            },
        )
        .await;
    assert!(removed.task.removed_at.is_some());

    handle.shutdown().await;
}

#[tokio::test]
async fn task_state_survives_daemon_restart() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();

    let handle = spawn_daemon(&state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut orch = Client::connect(handle.socket_path());
    let _: StatusResponse = orch
        .request(MessageType::Register, &register_payload("orch"))
        .await;
    let created: TaskResponse = orch
        .request(
            MessageType::CreateTask,
            &CreateTaskPayload {
                title: "persist me".into(),
                description: "note".into(),
                assignee: "worker".into(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: "urgent".into(),
                stale_after_seconds: 0,
            },
        )
        .await;
    drop(orch);
    handle.shutdown().await;

    let handle2 = spawn_daemon(&state_dir).await;
    wait_for_socket(handle2.socket_path()).await;
    let mut orch = Client::connect(handle2.socket_path());
    let _: StatusResponse = orch
        .request(MessageType::Register, &register_payload("orch"))
        .await;
    let listed: ListTasksResponse = orch
        .request(
            MessageType::ListTasks,
            &ListTasksPayload {
                assignee: String::new(),
                created_by: String::new(),
                status: None,
            },
        )
        .await;
    assert_eq!(listed.tasks.len(), 1);
    assert_eq!(listed.tasks[0].id, created.task.id);
    assert_eq!(listed.tasks[0].title, "persist me");

    handle2.shutdown().await;
}
