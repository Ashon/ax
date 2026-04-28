//! Runtime status metrics emission for ax-managed agent sessions.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use ax_proto::payloads::{RegisterPayload, UpdateAgentStatusMetricsPayload};
use ax_proto::responses::{AgentStatusMetricsResponse, StatusResponse};
use ax_proto::types::{
    AgentStatusFreshness, AgentStatusMetrics, AgentStatusSourceQuality, AgentWorkState,
};
use ax_proto::{Envelope, ErrorPayload, MessageType, ResponsePayload};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::{claude_project_path, Runtime};

const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(200);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const TMUX_STATUS_OPTION: &str = "@ax-status-title";

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RuntimeMetricSnapshot {
    pub session_id: String,
    pub context_tokens: Option<i64>,
    pub context_window: Option<i64>,
    pub usage_ratio: Option<f64>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub compact_eligible: Option<bool>,
}

impl RuntimeMetricSnapshot {
    fn sanitized(&self) -> Self {
        Self {
            session_id: self.session_id.trim().to_owned(),
            context_tokens: non_negative(self.context_tokens),
            context_window: positive(self.context_window),
            usage_ratio: valid_ratio(self.usage_ratio),
            last_activity_at: self.last_activity_at,
            compact_eligible: self.compact_eligible,
        }
    }
}

pub(crate) trait RuntimeMetricSource {
    fn snapshot(&self) -> RuntimeMetricSnapshot;
}

#[derive(Debug, Clone)]
pub(crate) struct ClaudeMetricSource {
    project_dir: Option<PathBuf>,
}

impl ClaudeMetricSource {
    pub(crate) fn from_workspace_dir(dir: &Path) -> Self {
        Self {
            project_dir: claude_project_path(dir).ok(),
        }
    }
}

impl RuntimeMetricSource for ClaudeMetricSource {
    fn snapshot(&self) -> RuntimeMetricSnapshot {
        let Some(project_dir) = &self.project_dir else {
            return RuntimeMetricSnapshot::default();
        };
        let Some(path) = latest_jsonl_under(project_dir) else {
            return RuntimeMetricSnapshot::default();
        };
        scan_claude_transcript(&path).unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CodexMetricSource {
    codex_home: PathBuf,
}

impl CodexMetricSource {
    pub(crate) fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }
}

impl RuntimeMetricSource for CodexMetricSource {
    fn snapshot(&self) -> RuntimeMetricSnapshot {
        let sessions = self.codex_home.join("sessions");
        let Some(path) = latest_jsonl_under(&sessions) else {
            return RuntimeMetricSnapshot::default();
        };
        scan_codex_session(&path).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct EmittedMetricState {
    work_state: AgentWorkState,
    snapshot: RuntimeMetricSnapshot,
}

pub(crate) trait StatusMetricsClient {
    fn update_agent_status_metrics(
        &mut self,
        payload: &UpdateAgentStatusMetricsPayload,
    ) -> Result<AgentStatusMetrics, StatusReporterError>;
}

pub(crate) trait TmuxTitleUpdater {
    fn set_status_title(&mut self, workspace: &str, title: &str)
        -> Result<(), StatusReporterError>;
}

pub(crate) struct RuntimeStatusReporter<C = DaemonStatusClient, T = TmuxStatusTitle> {
    runtime: Runtime,
    workspace: String,
    client: Option<C>,
    tmux: T,
    last_emitted: Option<EmittedMetricState>,
}

impl RuntimeStatusReporter<DaemonStatusClient, TmuxStatusTitle> {
    pub(crate) fn new(
        runtime: Runtime,
        workspace: &str,
        dir: &Path,
        socket_path: &Path,
        config_path: Option<&Path>,
    ) -> Self {
        let client =
            DaemonStatusClient::connect(runtime, socket_path, workspace, dir, config_path).ok();
        Self {
            runtime,
            workspace: workspace.to_owned(),
            client,
            tmux: TmuxStatusTitle,
            last_emitted: None,
        }
    }
}

impl<C, T> RuntimeStatusReporter<C, T>
where
    C: StatusMetricsClient,
    T: TmuxTitleUpdater,
{
    #[cfg(test)]
    fn with_parts(runtime: Runtime, workspace: &str, client: Option<C>, tmux: T) -> Self {
        Self {
            runtime,
            workspace: workspace.to_owned(),
            client,
            tmux,
            last_emitted: None,
        }
    }

    fn emit(&mut self, work_state: AgentWorkState, snapshot: &RuntimeMetricSnapshot, force: bool) {
        let snapshot = snapshot.sanitized();
        let state = EmittedMetricState {
            work_state: work_state.clone(),
            snapshot: snapshot.clone(),
        };
        if !force && self.last_emitted.as_ref() == Some(&state) {
            return;
        }

        let payload = self.payload_for(work_state, &snapshot);
        let local_title = self.local_status_title(&payload);
        let title = self
            .client
            .as_mut()
            .and_then(|client| client.update_agent_status_metrics(&payload).ok())
            .map_or(local_title, |metrics| metrics.status_title);

        let _ = self.tmux.set_status_title(&self.workspace, &title);
        self.last_emitted = Some(state);
    }

    fn payload_for(
        &self,
        work_state: AgentWorkState,
        snapshot: &RuntimeMetricSnapshot,
    ) -> UpdateAgentStatusMetricsPayload {
        UpdateAgentStatusMetricsPayload {
            workspace: self.workspace.clone(),
            agent: self.workspace.clone(),
            runtime_id: self.runtime.as_str().to_owned(),
            runtime_name: runtime_display_name(self.runtime).to_owned(),
            session_id: snapshot.session_id.clone(),
            context_tokens: snapshot.context_tokens,
            context_window: snapshot.context_window,
            usage_ratio: snapshot.usage_ratio,
            last_activity_at: snapshot.last_activity_at.or_else(|| Some(Utc::now())),
            work_state,
            compact_eligible: snapshot.compact_eligible,
            freshness: AgentStatusFreshness::Fresh,
            source_quality: AgentStatusSourceQuality::Runtime,
        }
    }

    fn local_status_title(&self, payload: &UpdateAgentStatusMetricsPayload) -> String {
        AgentStatusMetrics {
            workspace: self.workspace.clone(),
            agent: payload.agent.clone(),
            runtime_id: payload.runtime_id.clone(),
            runtime_name: payload.runtime_name.clone(),
            session_id: payload.session_id.clone(),
            context_tokens: payload.context_tokens,
            context_window: payload.context_window,
            usage_ratio: payload.usage_ratio,
            last_activity_at: payload.last_activity_at,
            work_state: payload.work_state.clone(),
            compact_eligible: payload.compact_eligible,
            freshness: payload.freshness.clone(),
            source_quality: payload.source_quality.clone(),
            updated_at: None,
            status_title: String::new(),
        }
        .formatted_status_title()
    }
}

pub(crate) fn run_command_with_status<S>(
    cmd: &mut Command,
    reporter: &mut RuntimeStatusReporter,
    source: &S,
) -> io::Result<ExitStatus>
where
    S: RuntimeMetricSource,
{
    let mut child = cmd.spawn()?;
    reporter.emit(AgentWorkState::Busy, &source.snapshot(), true);

    let mut next_refresh = Instant::now() + STATUS_REFRESH_INTERVAL;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let now = Instant::now();
        if now >= next_refresh {
            reporter.emit(AgentWorkState::Busy, &source.snapshot(), false);
            next_refresh = now + STATUS_REFRESH_INTERVAL;
        }
        thread::sleep(CHILD_POLL_INTERVAL);
    };

    reporter.emit(AgentWorkState::Idle, &source.snapshot(), true);
    Ok(status)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StatusReporterError {
    #[error("connect {socket_path}: {source}")]
    Connect {
        socket_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("encode envelope: {0}")]
    EncodeEnvelope(#[from] serde_json::Error),
    #[error("write request: {0}")]
    Write(io::Error),
    #[error("read response: {0}")]
    Read(io::Error),
    #[error("connection closed before response arrived")]
    ConnectionClosed,
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("unexpected envelope {0:?}")]
    UnexpectedEnvelope(MessageType),
    #[error("tmux {op}: {message}")]
    TmuxCommand { op: String, message: String },
    #[error("tmux exec: {0}")]
    TmuxIo(#[from] io::Error),
}

pub(crate) struct DaemonStatusClient {
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl DaemonStatusClient {
    fn connect(
        runtime: Runtime,
        socket_path: &Path,
        workspace: &str,
        dir: &Path,
        config_path: Option<&Path>,
    ) -> Result<Self, StatusReporterError> {
        let stream =
            UnixStream::connect(socket_path).map_err(|source| StatusReporterError::Connect {
                socket_path: socket_path.to_path_buf(),
                source,
            })?;
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(StatusReporterError::Read)?;
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(StatusReporterError::Write)?;

        let mut client = Self {
            reader: BufReader::new(stream),
            next_id: 1,
        };

        let _: StatusResponse = client.request(
            MessageType::Register,
            &RegisterPayload {
                workspace: workspace.to_owned(),
                dir: dir.display().to_string(),
                description: format!("{} runtime launcher", runtime_display_name(runtime)),
                config_path: config_path
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                idle_timeout_seconds: 0,
            },
        )?;
        Ok(client)
    }

    fn request<P, R>(&mut self, kind: MessageType, payload: &P) -> Result<R, StatusReporterError>
    where
        P: serde::Serialize,
        R: DeserializeOwned,
    {
        let request_id = format!("agent-status-{}", self.next_id);
        self.next_id += 1;

        let env = Envelope::new(&request_id, kind, payload)?;
        let mut bytes = serde_json::to_vec(&env)?;
        bytes.push(b'\n');
        self.reader
            .get_mut()
            .write_all(&bytes)
            .map_err(StatusReporterError::Write)?;
        self.reader
            .get_mut()
            .flush()
            .map_err(StatusReporterError::Write)?;

        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(StatusReporterError::Read)?;
            if read == 0 {
                return Err(StatusReporterError::ConnectionClosed);
            }
            let env: Envelope = serde_json::from_str(line.trim_end())?;
            if env.id != request_id {
                continue;
            }
            match env.r#type {
                MessageType::Response => {
                    let payload: ResponsePayload = env.decode_payload()?;
                    return serde_json::from_str(payload.data.get()).map_err(Into::into);
                }
                MessageType::Error => {
                    let payload: ErrorPayload = env.decode_payload()?;
                    return Err(StatusReporterError::Daemon(payload.message));
                }
                other => return Err(StatusReporterError::UnexpectedEnvelope(other)),
            }
        }
    }
}

impl StatusMetricsClient for DaemonStatusClient {
    fn update_agent_status_metrics(
        &mut self,
        payload: &UpdateAgentStatusMetricsPayload,
    ) -> Result<AgentStatusMetrics, StatusReporterError> {
        let response: AgentStatusMetricsResponse =
            self.request(MessageType::UpdateAgentStatusMetrics, payload)?;
        Ok(response.metrics)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TmuxStatusTitle;

impl TmuxTitleUpdater for TmuxStatusTitle {
    fn set_status_title(
        &mut self,
        workspace: &str,
        title: &str,
    ) -> Result<(), StatusReporterError> {
        let session = ax_tmux::session_name(workspace);
        run_tmux(
            "rename-window",
            &["rename-window", "-t", session.as_str(), title],
        )?;
        run_tmux(
            "set-option status title",
            &[
                "set-option",
                "-q",
                "-t",
                session.as_str(),
                TMUX_STATUS_OPTION,
                title,
            ],
        )
    }
}

fn run_tmux(op: &str, args: &[&str]) -> Result<(), StatusReporterError> {
    let output = Command::new("tmux").args(args).output()?;
    if output.status.success() {
        return Ok(());
    }
    let mut combined = Vec::with_capacity(output.stdout.len() + output.stderr.len());
    combined.extend_from_slice(&output.stdout);
    combined.extend_from_slice(&output.stderr);
    Err(StatusReporterError::TmuxCommand {
        op: op.to_owned(),
        message: String::from_utf8_lossy(&combined).trim().to_owned(),
    })
}

fn runtime_display_name(runtime: Runtime) -> &'static str {
    match runtime {
        Runtime::Claude => "Claude",
        Runtime::Codex => "Codex",
    }
}

fn non_negative(value: Option<i64>) -> Option<i64> {
    value.filter(|v| *v >= 0)
}

fn positive(value: Option<i64>) -> Option<i64> {
    value.filter(|v| *v > 0)
}

fn valid_ratio(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
}

fn latest_jsonl_under(root: &Path) -> Option<PathBuf> {
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    collect_latest_jsonl(root, &mut latest);
    latest.map(|(_, path)| path)
}

fn collect_latest_jsonl(dir: &Path, latest: &mut Option<(SystemTime, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_latest_jsonl(&path, latest);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let replace = latest.as_ref().is_none_or(|(prev, prev_path)| {
            modified > *prev || (modified == *prev && path < *prev_path)
        });
        if replace {
            *latest = Some((modified, path));
        }
    }
}

fn scan_claude_transcript(path: &Path) -> io::Result<RuntimeMetricSnapshot> {
    let file = fs::File::open(path)?;
    let mut snapshot = RuntimeMetricSnapshot::default();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<ClaudeTranscriptRecord>(&line) else {
            continue;
        };
        if !record.session_id.is_empty() {
            snapshot.session_id = record.session_id;
        }
        if let Some(ts) = record.timestamp {
            snapshot.last_activity_at = Some(ts);
        }
        if let Some(usage) = record.message.and_then(|message| message.usage) {
            snapshot.context_tokens = usage.context_tokens();
        }
    }
    Ok(snapshot)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClaudeTranscriptRecord {
    #[serde(rename = "sessionId")]
    session_id: String,
    timestamp: Option<DateTime<Utc>>,
    message: Option<ClaudeMessage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClaudeMessage {
    usage: Option<ClaudeUsage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_field_names)] // JSON keys are runtime usage field names.
struct ClaudeUsage {
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
}

impl ClaudeUsage {
    fn context_tokens(&self) -> Option<i64> {
        checked_non_negative_sum(&[
            self.input_tokens,
            self.output_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        ])
    }
}

fn scan_codex_session(path: &Path) -> io::Result<RuntimeMetricSnapshot> {
    let file = fs::File::open(path)?;
    let mut snapshot = RuntimeMetricSnapshot::default();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<CodexRecord>(&line) else {
            continue;
        };
        if let Some(ts) = record.timestamp {
            snapshot.last_activity_at = Some(ts);
        }
        match record.line_type.as_str() {
            "session_meta" => {
                let meta: CodexSessionMeta =
                    serde_json::from_value(record.payload).unwrap_or_default();
                if !meta.id.is_empty() {
                    snapshot.session_id = meta.id;
                }
            }
            "event_msg" => {
                let event: CodexEvent = serde_json::from_value(record.payload).unwrap_or_default();
                if event.event_type != "token_count" {
                    continue;
                }
                let Some(info) = event.info else {
                    continue;
                };
                snapshot.context_tokens = info.total_token_usage.context_tokens();
            }
            _ => {}
        }
    }
    Ok(snapshot)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexRecord {
    timestamp: Option<DateTime<Utc>>,
    #[serde(rename = "type")]
    line_type: String,
    payload: serde_json::Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexSessionMeta {
    id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexEvent {
    #[serde(rename = "type")]
    event_type: String,
    info: Option<CodexTokenInfo>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexTokenInfo {
    total_token_usage: CodexTokenUsage,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_field_names)] // JSON keys are runtime usage field names.
struct CodexTokenUsage {
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    reasoning_output_tokens: i64,
    total_tokens: i64,
}

impl CodexTokenUsage {
    fn context_tokens(&self) -> Option<i64> {
        if self.total_tokens > 0 {
            return Some(self.total_tokens);
        }
        checked_non_negative_sum(&[
            self.input_tokens,
            self.cached_input_tokens,
            self.output_tokens,
            self.reasoning_output_tokens,
        ])
        .filter(|total| *total > 0)
    }
}

fn checked_non_negative_sum(values: &[i64]) -> Option<i64> {
    values
        .iter()
        .try_fold(0_i64, |acc, value| {
            if *value < 0 {
                None
            } else {
                acc.checked_add(*value)
            }
        })
        .filter(|total| *total >= 0)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone)]
    struct FakeClient {
        updates: Rc<RefCell<Vec<UpdateAgentStatusMetricsPayload>>>,
        fail: bool,
    }

    impl StatusMetricsClient for FakeClient {
        fn update_agent_status_metrics(
            &mut self,
            payload: &UpdateAgentStatusMetricsPayload,
        ) -> Result<AgentStatusMetrics, StatusReporterError> {
            self.updates.borrow_mut().push(payload.clone());
            if self.fail {
                return Err(StatusReporterError::Daemon("rejected".into()));
            }
            Ok(AgentStatusMetrics {
                workspace: payload.workspace.clone(),
                agent: payload.agent.clone(),
                runtime_id: payload.runtime_id.clone(),
                runtime_name: payload.runtime_name.clone(),
                session_id: payload.session_id.clone(),
                context_tokens: payload.context_tokens,
                context_window: payload.context_window,
                usage_ratio: payload.usage_ratio,
                last_activity_at: payload.last_activity_at,
                work_state: payload.work_state.clone(),
                compact_eligible: payload.compact_eligible,
                freshness: payload.freshness.clone(),
                source_quality: payload.source_quality.clone(),
                updated_at: None,
                status_title: payload_status_title(payload),
            })
        }
    }

    #[derive(Clone, Default)]
    struct FakeTmux {
        titles: Rc<RefCell<Vec<(String, String)>>>,
    }

    impl TmuxTitleUpdater for FakeTmux {
        fn set_status_title(
            &mut self,
            workspace: &str,
            title: &str,
        ) -> Result<(), StatusReporterError> {
            self.titles
                .borrow_mut()
                .push((workspace.to_owned(), title.to_owned()));
            Ok(())
        }
    }

    #[test]
    fn reporter_emits_unknown_context_without_fabricating_numbers() {
        let updates = Rc::new(RefCell::new(Vec::new()));
        let titles = Rc::new(RefCell::new(Vec::new()));
        let client = FakeClient {
            updates: updates.clone(),
            fail: false,
        };
        let tmux = FakeTmux {
            titles: titles.clone(),
        };
        let mut reporter =
            RuntimeStatusReporter::with_parts(Runtime::Codex, "worker", Some(client), tmux);

        reporter.emit(
            AgentWorkState::Busy,
            &RuntimeMetricSnapshot::default(),
            true,
        );

        let payloads = updates.borrow();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].context_tokens, None);
        assert_eq!(payloads[0].context_window, None);
        assert_eq!(payloads[0].usage_ratio, None);
        assert_eq!(payloads[0].compact_eligible, None);
        assert_eq!(payloads[0].work_state, AgentWorkState::Busy);
        assert_eq!(
            titles.borrow()[0],
            (
                "worker".to_owned(),
                "ax:worker ctx=?/? ?% busy compact=?".to_owned()
            )
        );
    }

    #[test]
    fn reporter_sanitizes_values_before_daemon_validation() {
        let updates = Rc::new(RefCell::new(Vec::new()));
        let client = FakeClient {
            updates: updates.clone(),
            fail: false,
        };
        let mut reporter = RuntimeStatusReporter::with_parts(
            Runtime::Claude,
            "worker",
            Some(client),
            FakeTmux::default(),
        );

        reporter.emit(
            AgentWorkState::Busy,
            &RuntimeMetricSnapshot {
                context_tokens: Some(-1),
                context_window: Some(0),
                usage_ratio: Some(1.2),
                ..RuntimeMetricSnapshot::default()
            },
            true,
        );

        let payload = &updates.borrow()[0];
        assert_eq!(payload.context_tokens, None);
        assert_eq!(payload.context_window, None);
        assert_eq!(payload.usage_ratio, None);
    }

    #[test]
    fn reporter_sets_local_title_when_daemon_update_fails() {
        let updates = Rc::new(RefCell::new(Vec::new()));
        let titles = Rc::new(RefCell::new(Vec::new()));
        let client = FakeClient {
            updates,
            fail: true,
        };
        let tmux = FakeTmux {
            titles: titles.clone(),
        };
        let mut reporter =
            RuntimeStatusReporter::with_parts(Runtime::Codex, "worker", Some(client), tmux);

        reporter.emit(
            AgentWorkState::Busy,
            &RuntimeMetricSnapshot {
                context_tokens: Some(42_000),
                context_window: Some(100_000),
                ..RuntimeMetricSnapshot::default()
            },
            true,
        );

        assert_eq!(
            titles.borrow()[0].1,
            "ax:worker ctx=42k/100k 42% busy compact=?"
        );
    }

    #[test]
    fn codex_source_reads_latest_session_metrics() {
        let temp = tempfile::tempdir().unwrap();
        let rollout = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("04")
            .join("28")
            .join("rollout.jsonl");
        fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        fs::write(
            &rollout,
            r#"{"timestamp":"2026-04-28T07:00:00Z","type":"session_meta","payload":{"id":"sess-1","cwd":"/tmp/ws"}}
{"timestamp":"2026-04-28T07:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5,"total_tokens":155},"last_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":30,"reasoning_output_tokens":0,"total_tokens":130}}}}
"#,
        )
        .unwrap();

        let source = CodexMetricSource::new(temp.path().to_path_buf());
        let snapshot = source.snapshot();
        assert_eq!(snapshot.session_id, "sess-1");
        assert_eq!(snapshot.context_tokens, Some(155));
        assert_eq!(
            snapshot.last_activity_at.unwrap().to_rfc3339(),
            "2026-04-28T07:01:00+00:00"
        );
    }

    #[test]
    fn claude_source_reads_latest_usage_metrics() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("chat.jsonl");
        fs::write(
            &transcript,
            r#"{"type":"assistant","sessionId":"claude-sess","timestamp":"2026-04-28T07:02:00Z","message":{"usage":{"input_tokens":1000,"output_tokens":200,"cache_read_input_tokens":30,"cache_creation_input_tokens":40}}}
"#,
        )
        .unwrap();
        let source = ClaudeMetricSource {
            project_dir: Some(temp.path().to_path_buf()),
        };

        let snapshot = source.snapshot();
        assert_eq!(snapshot.session_id, "claude-sess");
        assert_eq!(snapshot.context_tokens, Some(1270));
        assert_eq!(
            snapshot.last_activity_at.unwrap().to_rfc3339(),
            "2026-04-28T07:02:00+00:00"
        );
    }

    fn payload_status_title(payload: &UpdateAgentStatusMetricsPayload) -> String {
        AgentStatusMetrics {
            workspace: payload.workspace.clone(),
            agent: payload.agent.clone(),
            runtime_id: payload.runtime_id.clone(),
            runtime_name: payload.runtime_name.clone(),
            session_id: payload.session_id.clone(),
            context_tokens: payload.context_tokens,
            context_window: payload.context_window,
            usage_ratio: payload.usage_ratio,
            last_activity_at: payload.last_activity_at,
            work_state: payload.work_state.clone(),
            compact_eligible: payload.compact_eligible,
            freshness: payload.freshness.clone(),
            source_quality: payload.source_quality.clone(),
            updated_at: None,
            status_title: String::new(),
        }
        .formatted_status_title()
    }
}
