//! Async client that brokers envelopes between an MCP tool handler
//! and the ax daemon's Unix socket. A single reader task demultiplexes
//! responses back to per-request oneshot channels keyed by envelope
//! id, with a separate bucket for push envelopes that tools can drain
//! via [`DaemonClient::take_push_messages`].
//!
//! Callers invoke `request()` / `request_json()` which await the
//! matching oneshot receiver with a configurable per-request timeout.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{oneshot, Mutex as TokioMutex};
use tokio::task::JoinHandle;
use uuid::Uuid;

use ax_daemon::expand_socket_path;
use ax_proto::payloads::RegisterPayload;
use ax_proto::types::Message;
use ax_proto::{Envelope, ErrorPayload, MessageType, ResponsePayload};

/// Default per-request timeout when the caller doesn't supply one.
/// Matches `DefaultRequestTimeout`.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum DaemonClientError {
    #[error("connect to daemon {path:?}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("daemon connection closed")]
    Disconnected,
    #[error("request {kind:?} timed out after {elapsed:?}")]
    Timeout {
        kind: MessageType,
        elapsed: Duration,
    },
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("encode envelope: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("write envelope: {0}")]
    Write(#[source] std::io::Error),
    #[error("register: {0}")]
    Register(String),
}

/// Opaque handle to a connected daemon client. Clone cheaply — every
/// field is behind an `Arc`.
#[derive(Clone)]
pub struct DaemonClient {
    socket_path: PathBuf,
    workspace: String,
    registration: Arc<Mutex<Registration>>,
    writer: Arc<TokioMutex<tokio::net::unix::OwnedWriteHalf>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<RequestResult>>>>,
    pushes: Arc<Mutex<Vec<Message>>>,
    reader_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    request_timeout: Arc<Mutex<Duration>>,
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient")
            .field("socket_path", &self.socket_path)
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default, Clone)]
struct Registration {
    dir: String,
    description: String,
    config_path: String,
    idle_timeout: Duration,
}

enum RequestResult {
    Ok(Envelope),
    Disconnected,
}

impl DaemonClient {
    /// Prepare a client without connecting. Call
    /// [`DaemonClientBuilder::connect`] to actually open the socket
    /// and register with the daemon.
    #[must_use]
    pub fn builder(
        socket_path: impl AsRef<Path>,
        workspace: impl Into<String>,
    ) -> DaemonClientBuilder {
        let as_str = socket_path.as_ref().to_string_lossy();
        DaemonClientBuilder {
            socket_path: expand_socket_path(as_str.as_ref()),
            workspace: workspace.into(),
            registration: Registration::default(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Override the per-request timeout. A zero or negative duration
    /// disables the bound. Intended for tests.
    pub fn set_request_timeout(&self, timeout: Duration) {
        *self
            .request_timeout
            .lock()
            .expect("request_timeout poisoned") = timeout;
    }

    /// Drain and return every push envelope received since the last
    /// call. Tools use this to surface `push_message` envelopes out of
    /// band from synchronous `read_messages` polls.
    #[must_use]
    pub fn take_push_messages(&self) -> Vec<Message> {
        let mut guard = self.pushes.lock().expect("pushes poisoned");
        std::mem::take(&mut *guard)
    }

    /// Dispatch `payload` as a new envelope of type `kind` and
    /// deserialize the response `data` field into `R`.
    pub async fn request<P, R>(
        &self,
        kind: MessageType,
        payload: &P,
    ) -> Result<R, DaemonClientError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let resp = self.request_raw(kind.clone(), payload).await?;
        decode_response_data(&resp)
    }

    /// Send an envelope and return the decoded `ResponsePayload` as
    /// raw `serde_json::Value`. Used by tools that want to forward
    /// the body to their own schema.
    pub async fn request_value<P>(
        &self,
        kind: MessageType,
        payload: &P,
    ) -> Result<serde_json::Value, DaemonClientError>
    where
        P: Serialize,
    {
        let resp = self.request_raw(kind, payload).await?;
        let wrap: ResponsePayload = resp.decode_payload().map_err(DaemonClientError::Encode)?;
        Ok(serde_json::from_str(wrap.data.get())?)
    }

    async fn request_raw<P>(
        &self,
        kind: MessageType,
        payload: &P,
    ) -> Result<Envelope, DaemonClientError>
    where
        P: Serialize,
    {
        let id = Uuid::new_v4().to_string();
        let env = Envelope::new(&id, kind.clone(), payload)?;

        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending poisoned")
            .insert(id.clone(), tx);

        let mut bytes = serde_json::to_vec(&env)?;
        bytes.push(b'\n');
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(&bytes).await {
                self.pending.lock().expect("pending poisoned").remove(&id);
                return Err(DaemonClientError::Write(e));
            }
        }

        let timeout = *self
            .request_timeout
            .lock()
            .expect("request_timeout poisoned");
        let result = if timeout.is_zero() {
            rx.await.map_err(|_| DaemonClientError::Disconnected)?
        } else {
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(r)) => r,
                Ok(Err(_)) => return Err(DaemonClientError::Disconnected),
                Err(_) => {
                    self.pending.lock().expect("pending poisoned").remove(&id);
                    return Err(DaemonClientError::Timeout {
                        kind,
                        elapsed: timeout,
                    });
                }
            }
        };
        match result {
            RequestResult::Ok(env) => match env.r#type {
                MessageType::Error => {
                    let err: ErrorPayload =
                        env.decode_payload().map_err(DaemonClientError::Encode)?;
                    Err(DaemonClientError::Daemon(err.message))
                }
                _ => Ok(env),
            },
            RequestResult::Disconnected => Err(DaemonClientError::Disconnected),
        }
    }

    /// Close the connection. Outstanding `request` calls error with
    /// [`DaemonClientError::Disconnected`].
    pub async fn close(&self) {
        // Drain all pending and notify disconnect.
        let pending: Vec<oneshot::Sender<RequestResult>> = {
            let mut guard = self.pending.lock().expect("pending poisoned");
            guard.drain().map(|(_, tx)| tx).collect()
        };
        for tx in pending {
            let _ = tx.send(RequestResult::Disconnected);
        }
        let handle = self
            .reader_task
            .lock()
            .expect("reader_task poisoned")
            .take();
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
    }
}

/// Builder returned by [`DaemonClient::new`]. Lets callers set the
/// registration info + request timeout before opening the socket.
pub struct DaemonClientBuilder {
    socket_path: PathBuf,
    workspace: String,
    registration: Registration,
    request_timeout: Duration,
}

impl DaemonClientBuilder {
    #[must_use]
    pub fn dir(mut self, dir: impl Into<String>) -> Self {
        self.registration.dir = dir.into();
        self
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.registration.description = description.into();
        self
    }

    #[must_use]
    pub fn config_path(mut self, config_path: impl Into<String>) -> Self {
        self.registration.config_path = config_path.into();
        self
    }

    #[must_use]
    pub fn idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.registration.idle_timeout = idle_timeout;
        self
    }

    #[must_use]
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Connect to the daemon, start the reader task, and register
    /// the workspace. Returns a ready-to-use client.
    pub async fn connect(self) -> Result<DaemonClient, DaemonClientError> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|source| DaemonClientError::Connect {
                path: self.socket_path.clone(),
                source,
            })?;
        let (read_half, write_half) = stream.into_split();
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<RequestResult>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pushes = Arc::new(Mutex::new(Vec::new()));
        let reader_task = spawn_reader_loop(read_half, pending.clone(), pushes.clone());
        let client = DaemonClient {
            socket_path: self.socket_path.clone(),
            workspace: self.workspace.clone(),
            registration: Arc::new(Mutex::new(self.registration.clone())),
            writer: Arc::new(TokioMutex::new(write_half)),
            pending,
            pushes,
            reader_task: Arc::new(Mutex::new(Some(reader_task))),
            request_timeout: Arc::new(Mutex::new(self.request_timeout)),
        };

        // Resolve dir: prefer explicit setting, fall back to cwd.
        let dir = {
            let reg = client
                .registration
                .lock()
                .expect("registration poisoned")
                .clone();
            if reg.dir.is_empty() {
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            } else {
                reg.dir
            }
        };

        let reg = client
            .registration
            .lock()
            .expect("registration poisoned")
            .clone();
        client
            .request_raw(
                MessageType::Register,
                &RegisterPayload {
                    workspace: self.workspace.clone(),
                    dir,
                    description: reg.description,
                    config_path: reg.config_path,
                    idle_timeout_seconds: reg.idle_timeout.as_secs() as i64,
                },
            )
            .await
            .map_err(|e| DaemonClientError::Register(e.to_string()))?;

        Ok(client)
    }
}

fn spawn_reader_loop(
    read_half: tokio::net::unix::OwnedReadHalf,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<RequestResult>>>>,
    pushes: Arc<Mutex<Vec<Message>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "mcp daemon client read failed");
                    break;
                }
            }
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                continue;
            }
            let env: Envelope = match serde_json::from_str(trimmed) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "decode envelope failed");
                    continue;
                }
            };
            match env.r#type {
                MessageType::PushMessage => {
                    if let Ok(msg) = env.decode_payload::<Message>() {
                        pushes.lock().expect("pushes poisoned").push(msg);
                    }
                }
                MessageType::Response | MessageType::Error => {
                    if let Some(tx) = pending.lock().expect("pending poisoned").remove(&env.id) {
                        let _ = tx.send(RequestResult::Ok(env));
                    }
                }
                _ => {}
            }
        }
        // Drain pending with disconnect signal so outstanding requests fail fast.
        let drained: Vec<oneshot::Sender<RequestResult>> = pending
            .lock()
            .expect("pending poisoned")
            .drain()
            .map(|(_, tx)| tx)
            .collect();
        for tx in drained {
            let _ = tx.send(RequestResult::Disconnected);
        }
    })
}

fn decode_response_data<R: DeserializeOwned>(env: &Envelope) -> Result<R, DaemonClientError> {
    let wrap: ResponsePayload = env.decode_payload().map_err(DaemonClientError::Encode)?;
    serde_json::from_str(wrap.data.get()).map_err(DaemonClientError::Encode)
}
