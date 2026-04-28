//! Minimal sync daemon client used by the TUI. Mirrors the piece of
//! `ax-cli::daemon_client` the TUI needs — register as `_watch`, list
//! workspaces, list tasks. The TUI prefers reading state files
//! (`tasks.json`, `message_history.jsonl`) directly; this client is
//! only used when a live snapshot is cheaper than re-scanning a
//! potentially stale file.

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use ax_proto::payloads::{
    CancelTaskPayload, CreateTaskPayload, InterveneTaskPayload, RegisterPayload,
    SendMessagePayload, UsageTrendWorkspace, UsageTrendsPayload,
};
use ax_proto::responses::{
    InterveneTaskResponse, ListWorkspacesResponse, SendMessageResponse, StatusResponse,
    TaskResponse, UsageTrendsResponse,
};
use ax_proto::types::{Task, WorkspaceInfo};
use ax_proto::usage::WorkspaceTrend;
use ax_proto::{Envelope, ErrorPayload, MessageType, ResponsePayload};
use serde::de::DeserializeOwned;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub(crate) enum DaemonClientError {
    #[error("connect {path}: {source}")]
    Connect {
        path: String,
        source: std::io::Error,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode envelope: {0}")]
    Encode(serde_json::Error),
    #[error("decode response: {0}")]
    Decode(serde_json::Error),
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("connection closed before response arrived")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::thread;

    use ax_proto::payloads::{CreateTaskPayload, RegisterPayload, SendMessagePayload};
    use ax_proto::responses::{SendMessageResponse, StatusResponse, TaskResponse};
    use ax_proto::types::{Task, TaskStartMode, TaskStatus};
    use chrono::Utc;
    use tempfile::TempDir;

    #[test]
    fn create_task_sends_wire_payload_and_decodes_response() {
        let temp = TempDir::new().expect("tempdir");
        let socket_path = temp.path().join("ax.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind fake daemon");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept client");
            let mut reader = BufReader::new(stream);

            let register = read_env(&mut reader);
            assert_eq!(register.r#type, MessageType::Register);
            let register_payload: RegisterPayload =
                register.decode_payload().expect("decode register");
            assert_eq!(register_payload.workspace, "_cli");
            write_response(
                reader.get_mut(),
                &register.id,
                &StatusResponse {
                    status: "ok".into(),
                },
            );

            let create = read_env(&mut reader);
            assert_eq!(create.r#type, MessageType::CreateTask);
            let create_payload: CreateTaskPayload =
                create.decode_payload().expect("decode create_task");
            assert_eq!(create_payload.title, "Ship top task creation");
            assert_eq!(create_payload.description, "from tui test");
            assert_eq!(create_payload.assignee, "alpha");
            assert!(create_payload.priority.is_empty());
            assert!(create_payload.start_mode.is_empty());
            assert!(create_payload.workflow_mode.is_empty());

            write_response(
                reader.get_mut(),
                &create.id,
                &TaskResponse {
                    task: task_from_payload(&create_payload, "_cli"),
                },
            );
        });

        let mut client = Client::connect_as(&socket_path, "_cli").expect("connect");
        let task = client
            .create_task("Ship top task creation", "from tui test", "alpha")
            .expect("create task");
        assert_eq!(task.title, "Ship top task creation");
        assert_eq!(task.description, "from tui test");
        assert_eq!(task.assignee, "alpha");
        assert_eq!(task.created_by, "_cli");

        server.join().expect("server thread");
    }

    #[test]
    fn send_message_sends_plain_wire_payload_and_decodes_response() {
        let temp = TempDir::new().expect("tempdir");
        let socket_path = temp.path().join("ax.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind fake daemon");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept client");
            let mut reader = BufReader::new(stream);

            let register = read_env(&mut reader);
            assert_eq!(register.r#type, MessageType::Register);
            let register_payload: RegisterPayload =
                register.decode_payload().expect("decode register");
            assert_eq!(register_payload.workspace, "_cli");
            write_response(
                reader.get_mut(),
                &register.id,
                &StatusResponse {
                    status: "ok".into(),
                },
            );

            let send = read_env(&mut reader);
            assert_eq!(send.r#type, MessageType::SendMessage);
            let payload: SendMessagePayload = send.decode_payload().expect("decode send_message");
            assert_eq!(payload.to, "orchestrator");
            assert_eq!(payload.message, "check queue health");
            assert_eq!(payload.config_path, "/tmp/ax.yaml");

            write_response(
                reader.get_mut(),
                &send.id,
                &SendMessageResponse {
                    message_id: "msg-1".into(),
                    status: "sent".into(),
                },
            );
        });

        let mut client = Client::connect_as(&socket_path, "_cli").expect("connect");
        let response = client
            .send_message(
                "orchestrator",
                "check queue health",
                Some(Path::new("/tmp/ax.yaml")),
            )
            .expect("send message");
        assert_eq!(response.message_id, "msg-1");
        assert_eq!(response.status, "sent");

        server.join().expect("server thread");
    }

    fn read_env(reader: &mut BufReader<UnixStream>) -> Envelope {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read envelope");
        serde_json::from_str(line.trim_end()).expect("decode envelope")
    }

    fn write_response<T: serde::Serialize>(stream: &mut UnixStream, id: &str, payload: &T) {
        let data = serde_json::value::RawValue::from_string(
            serde_json::to_string(payload).expect("encode response data"),
        )
        .expect("raw response data");
        let env = Envelope::new(
            id,
            MessageType::Response,
            &ResponsePayload {
                success: true,
                data,
            },
        )
        .expect("response envelope");
        let mut bytes = serde_json::to_vec(&env).expect("encode response envelope");
        bytes.push(b'\n');
        stream.write_all(&bytes).expect("write response");
        stream.flush().expect("flush response");
    }

    fn task_from_payload(payload: &CreateTaskPayload, created_by: &str) -> Task {
        let now = Utc::now();
        Task {
            id: "task-1".into(),
            title: payload.title.clone(),
            description: payload.description.clone(),
            assignee: payload.assignee.clone(),
            created_by: created_by.into(),
            parent_task_id: String::new(),
            child_task_ids: Vec::new(),
            version: 1,
            status: TaskStatus::Pending,
            start_mode: TaskStartMode::Default,
            workflow_mode: None,
            priority: None,
            stale_after_seconds: 0,
            dispatch_message: String::new(),
            dispatch_config_path: String::new(),
            dispatch_count: 0,
            attempt_count: 0,
            last_dispatch_at: None,
            last_attempt_at: None,
            next_retry_at: None,
            claimed_at: None,
            claimed_by: String::new(),
            claim_source: String::new(),
            result: String::new(),
            logs: Vec::new(),
            rollup: None,
            sequence: None,
            stale_info: None,
            removed_at: None,
            removed_by: String::new(),
            remove_reason: String::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

pub(crate) struct Client {
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Client {
    pub(crate) fn connect(socket_path: &Path) -> Result<Self, DaemonClientError> {
        Self::connect_as(socket_path, "_watch")
    }

    pub(crate) fn connect_as(
        socket_path: &Path,
        workspace: &str,
    ) -> Result<Self, DaemonClientError> {
        let stream =
            UnixStream::connect(socket_path).map_err(|source| DaemonClientError::Connect {
                path: socket_path.display().to_string(),
                source,
            })?;
        stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        let mut client = Self {
            reader: BufReader::new(stream),
            next_id: 1,
        };
        let dir = env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let _: StatusResponse = client.request(
            MessageType::Register,
            &RegisterPayload {
                workspace: workspace.to_owned(),
                dir,
                description: String::new(),
                config_path: String::new(),
                idle_timeout_seconds: 0,
            },
        )?;
        Ok(client)
    }

    pub(crate) fn list_workspaces(&mut self) -> Result<Vec<WorkspaceInfo>, DaemonClientError> {
        let response: ListWorkspacesResponse =
            self.request(MessageType::ListWorkspaces, &serde_json::json!({}))?;
        Ok(response.workspaces)
    }

    /// Ask the daemon to roll up token / turn totals from Claude and
    /// Codex transcripts for each `(name, cwd)` binding. The server
    /// still answers for offline workspaces — transcripts live on
    /// disk, not in the tmux session — so the TUI can surface
    /// historical usage once an agent has stopped.
    pub(crate) fn usage_trends(
        &mut self,
        bindings: &[(String, String)],
        since_minutes: i64,
        bucket_minutes: i64,
    ) -> Result<Vec<WorkspaceTrend>, DaemonClientError> {
        let payload = UsageTrendsPayload {
            workspaces: bindings
                .iter()
                .map(|(name, cwd)| UsageTrendWorkspace {
                    workspace: name.clone(),
                    cwd: cwd.clone(),
                })
                .collect(),
            since_minutes,
            bucket_minutes,
        };
        let response: UsageTrendsResponse = self.request(MessageType::UsageTrends, &payload)?;
        Ok(response.trends)
    }

    pub(crate) fn cancel_task(
        &mut self,
        id: &str,
        reason: &str,
        expected_version: Option<i64>,
    ) -> Result<Task, DaemonClientError> {
        let response: TaskResponse = self.request(
            MessageType::CancelTask,
            &CancelTaskPayload {
                id: id.to_owned(),
                reason: reason.to_owned(),
                expected_version,
            },
        )?;
        Ok(response.task)
    }

    pub(crate) fn create_task(
        &mut self,
        title: &str,
        description: &str,
        assignee: &str,
    ) -> Result<Task, DaemonClientError> {
        let response: TaskResponse = self.request(
            MessageType::CreateTask,
            &CreateTaskPayload {
                title: title.to_owned(),
                description: description.to_owned(),
                assignee: assignee.to_owned(),
                parent_task_id: String::new(),
                start_mode: String::new(),
                workflow_mode: String::new(),
                priority: String::new(),
                stale_after_seconds: 0,
            },
        )?;
        Ok(response.task)
    }

    pub(crate) fn send_message(
        &mut self,
        to: &str,
        message: &str,
        config_path: Option<&Path>,
    ) -> Result<SendMessageResponse, DaemonClientError> {
        self.request(
            MessageType::SendMessage,
            &SendMessagePayload {
                to: to.to_owned(),
                message: message.to_owned(),
                config_path: config_path
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            },
        )
    }

    pub(crate) fn intervene_task(
        &mut self,
        id: &str,
        action: &str,
        note: &str,
        expected_version: Option<i64>,
    ) -> Result<InterveneTaskResponse, DaemonClientError> {
        self.request(
            MessageType::InterveneTask,
            &InterveneTaskPayload {
                id: id.to_owned(),
                action: action.to_owned(),
                note: note.to_owned(),
                expected_version,
            },
        )
    }

    fn request<P, R>(&mut self, kind: MessageType, payload: &P) -> Result<R, DaemonClientError>
    where
        P: serde::Serialize,
        R: DeserializeOwned,
    {
        let id = format!("watch-{}", self.next_id);
        self.next_id += 1;
        let env = Envelope::new(&id, kind, payload).map_err(DaemonClientError::Encode)?;
        let mut bytes = serde_json::to_vec(&env).map_err(DaemonClientError::Encode)?;
        bytes.push(b'\n');
        self.reader.get_mut().write_all(&bytes)?;
        self.reader.get_mut().flush()?;
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line)?;
            if read == 0 {
                return Err(DaemonClientError::Closed);
            }
            let env: Envelope =
                serde_json::from_str(line.trim_end()).map_err(DaemonClientError::Decode)?;
            if env.id != id {
                continue;
            }
            match env.r#type {
                MessageType::Response => {
                    let wrap: ResponsePayload =
                        env.decode_payload().map_err(DaemonClientError::Decode)?;
                    return serde_json::from_str(wrap.data.get())
                        .map_err(DaemonClientError::Decode);
                }
                MessageType::Error => {
                    let err: ErrorPayload =
                        env.decode_payload().map_err(DaemonClientError::Decode)?;
                    return Err(DaemonClientError::Daemon(err.message));
                }
                _ => {
                    return Err(DaemonClientError::Daemon(format!(
                        "unexpected envelope {:?}",
                        env.r#type
                    )));
                }
            }
        }
    }
}
