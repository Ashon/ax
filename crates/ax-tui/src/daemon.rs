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
    CancelTaskPayload, InterveneTaskPayload, RegisterPayload, UsageTrendWorkspace,
    UsageTrendsPayload,
};
use ax_proto::responses::{
    InterveneTaskResponse, ListWorkspacesResponse, StatusResponse, TaskResponse,
    UsageTrendsResponse,
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
