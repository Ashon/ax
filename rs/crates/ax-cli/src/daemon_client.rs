use std::env;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ax_proto::payloads::{
    ListTasksPayload, ReadMessagesPayload, RegisterPayload, SendMessagePayload,
};
use ax_proto::responses::{
    ListTasksResponse, ListWorkspacesResponse, ReadMessagesResponse, SendMessageResponse,
    StatusResponse,
};
use ax_proto::types::{Message, Task, TaskStatus, WorkspaceInfo};
use ax_proto::{Envelope, ErrorPayload, MessageType, ResponsePayload};
use serde::de::DeserializeOwned;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) enum DaemonClientError {
    Connect {
        socket_path: PathBuf,
        source: io::Error,
    },
    ResolveCurrentDir(io::Error),
    EncodeEnvelope(serde_json::Error),
    Write(io::Error),
    Read(io::Error),
    ConnectionClosed,
    DecodeEnvelope(serde_json::Error),
    DecodeResponse(serde_json::Error),
    Daemon(String),
    UnexpectedEnvelope(MessageType),
}

impl fmt::Display for DaemonClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect {
                socket_path,
                source,
            } => {
                write!(f, "connect {}: {source}", socket_path.display())
            }
            Self::ResolveCurrentDir(source) => write!(f, "resolve current dir: {source}"),
            Self::EncodeEnvelope(source) => write!(f, "encode envelope: {source}"),
            Self::Write(source) => write!(f, "write request: {source}"),
            Self::Read(source) => write!(f, "read response: {source}"),
            Self::ConnectionClosed => f.write_str("connection closed before response arrived"),
            Self::DecodeEnvelope(source) => write!(f, "decode envelope: {source}"),
            Self::DecodeResponse(source) => write!(f, "decode response: {source}"),
            Self::Daemon(message) => write!(f, "daemon error: {message}"),
            Self::UnexpectedEnvelope(kind) => write!(f, "unexpected envelope {kind:?}"),
        }
    }
}

pub(crate) struct DaemonClient {
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl DaemonClient {
    pub(crate) fn connect(socket_path: &Path, workspace: &str) -> Result<Self, DaemonClientError> {
        let stream =
            UnixStream::connect(socket_path).map_err(|source| DaemonClientError::Connect {
                socket_path: socket_path.to_path_buf(),
                source,
            })?;
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(DaemonClientError::Read)?;
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(DaemonClientError::Write)?;

        let mut client = Self {
            reader: BufReader::new(stream),
            next_id: 1,
        };

        let dir = env::current_dir()
            .map_err(DaemonClientError::ResolveCurrentDir)?
            .display()
            .to_string();
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

    pub(crate) fn read_messages(
        &mut self,
        limit: i64,
        from: &str,
    ) -> Result<Vec<Message>, DaemonClientError> {
        let response: ReadMessagesResponse = self.request(
            MessageType::ReadMessages,
            &ReadMessagesPayload {
                limit,
                from: from.to_owned(),
            },
        )?;
        Ok(response.messages)
    }

    pub(crate) fn list_workspaces(&mut self) -> Result<Vec<WorkspaceInfo>, DaemonClientError> {
        let response: ListWorkspacesResponse =
            self.request(MessageType::ListWorkspaces, &serde_json::json!({}))?;
        Ok(response.workspaces)
    }

    pub(crate) fn list_tasks(
        &mut self,
        assignee: &str,
        created_by: &str,
        status: Option<TaskStatus>,
    ) -> Result<Vec<Task>, DaemonClientError> {
        let response: ListTasksResponse = self.request(
            MessageType::ListTasks,
            &ListTasksPayload {
                assignee: assignee.to_owned(),
                created_by: created_by.to_owned(),
                status,
            },
        )?;
        Ok(response.tasks)
    }

    fn request<P, R>(&mut self, kind: MessageType, payload: &P) -> Result<R, DaemonClientError>
    where
        P: serde::Serialize,
        R: DeserializeOwned,
    {
        let request_id = format!("cli-{}", self.next_id);
        self.next_id += 1;

        let env =
            Envelope::new(&request_id, kind, payload).map_err(DaemonClientError::EncodeEnvelope)?;
        let mut bytes = serde_json::to_vec(&env).map_err(DaemonClientError::EncodeEnvelope)?;
        bytes.push(b'\n');
        self.reader
            .get_mut()
            .write_all(&bytes)
            .map_err(DaemonClientError::Write)?;
        self.reader
            .get_mut()
            .flush()
            .map_err(DaemonClientError::Write)?;

        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(DaemonClientError::Read)?;
            if read == 0 {
                return Err(DaemonClientError::ConnectionClosed);
            }

            let env: Envelope =
                serde_json::from_str(line.trim_end()).map_err(DaemonClientError::DecodeEnvelope)?;
            if env.id != request_id {
                continue;
            }

            match env.r#type {
                MessageType::Response => {
                    let payload: ResponsePayload = env
                        .decode_payload()
                        .map_err(DaemonClientError::DecodeResponse)?;
                    return serde_json::from_str(payload.data.get())
                        .map_err(DaemonClientError::DecodeResponse);
                }
                MessageType::Error => {
                    let payload: ErrorPayload = env
                        .decode_payload()
                        .map_err(DaemonClientError::DecodeResponse)?;
                    return Err(DaemonClientError::Daemon(payload.message));
                }
                other => return Err(DaemonClientError::UnexpectedEnvelope(other)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ax_daemon::Daemon;
    use tempfile::TempDir;

    #[test]
    fn client_registers_sends_and_reads_messages() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let temp = TempDir::new().expect("tempdir");
        let socket_path = temp.path().join("ax.sock");
        let handle = runtime
            .block_on(Daemon::new(socket_path.clone()).bind())
            .expect("bind daemon");

        let mut alice = DaemonClient::connect(&socket_path, "alice").expect("connect alice");
        let mut bob = DaemonClient::connect(&socket_path, "bob").expect("connect bob");

        let response = alice
            .send_message("bob", "hello from alice", None)
            .expect("send message");
        assert_eq!(response.status, "sent");
        assert!(!response.message_id.is_empty());

        let messages = bob.read_messages(10, "").expect("read messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from, "alice");
        assert_eq!(messages[0].to, "bob");
        assert_eq!(messages[0].content, "hello from alice");

        let workspaces = alice.list_workspaces().expect("list workspaces");
        assert_eq!(workspaces.len(), 2);
        assert!(workspaces.iter().any(|ws| ws.name == "alice"));
        assert!(workspaces.iter().any(|ws| ws.name == "bob"));

        runtime.block_on(handle.shutdown());
    }
}
