//! Main event loop — owns the state and drives render/input/refresh.
//! Stays sync + single-threaded since ratatui doesn't push messages
//! through an async runtime.
//!
//! Refresh cadence is `watchDataRefreshInterval` (250ms): tmux
//! sessions re-listed, daemon re-queried for workspace info, view
//! redrawn.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, MouseEventKind};

use crate::actions::{Notice, QuickActionId};
use crate::daemon::Client;
use crate::state::{
    App, PendingOrchestratorMessage, PendingTaskAction, PendingTaskCreate, RefreshNoticeSource,
    ROOT_ORCHESTRATOR_WORKSPACE,
};
use crate::terminal::TerminalGuard;

const REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How often the TUI re-asks the daemon for historical token totals.
/// Slower than the main refresh tick because scanning transcripts
/// from disk is measurably more expensive than `list_workspaces`.
const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Rolling window the tokens panel asks the daemon to bucketise.
/// 24h × 60min keeps offline-but-recently-active agents visible
/// without dragging in weeks of stale data.
const USAGE_WINDOW_MINUTES: i64 = 24 * 60;
const USAGE_BUCKET_MINUTES: i64 = 5;

type TaskActionResult = Result<String, String>;
type MessageSendResult = Result<String, String>;

struct TaskActionJob {
    receiver: Receiver<TaskActionResult>,
}

struct MessageSendJob {
    receiver: Receiver<MessageSendResult>,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub socket_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("terminal setup: {0}")]
    Terminal(std::io::Error),
    #[error("render: {0}")]
    Render(std::io::Error),
    #[error("input: {0}")]
    Input(std::io::Error),
    #[error("tmux: {0}")]
    Tmux(ax_tmux::TmuxError),
}

pub fn run(opts: &RunOptions) -> Result<(), RunError> {
    let mut guard = TerminalGuard::install().map_err(RunError::Terminal)?;
    let mut app = App::new();
    let mut task_action_job = None;
    let mut message_send_job = None;
    refresh(&mut app, opts);

    loop {
        guard
            .terminal
            .draw(|f| crate::render::draw(f, &mut app))
            .map_err(RunError::Render)?;

        if event::poll(POLL_INTERVAL).map_err(RunError::Input)? {
            match event::read().map_err(RunError::Input)? {
                Event::Key(key) => crate::input::handle_key(&mut app, key),
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => crate::input::handle_scroll(&mut app, -1),
                    MouseEventKind::ScrollDown => crate::input::handle_scroll(&mut app, 1),
                    _ => {}
                },
                _ => {}
            }
        }
        if app.quit {
            break;
        }

        drain_pending_lifecycle(&mut app, opts);
        drain_pending_task_action(&mut app, opts, &mut task_action_job);
        drain_pending_task_create(&mut app, opts);
        drain_pending_orchestrator_message(&mut app, opts, &mut message_send_job);
        app.expire_notice();

        let due = app.force_refresh
            || app
                .last_refresh
                .is_none_or(|t| t.elapsed() >= REFRESH_INTERVAL);
        if due {
            app.force_refresh = false;
            app.tick_animation();
            refresh(&mut app, opts);
        }
    }
    Ok(())
}

fn drain_pending_lifecycle(app: &mut App, opts: &RunOptions) {
    let Some(pending) = app.pending_lifecycle.take() else {
        return;
    };
    let Ok(cwd) = std::env::current_dir() else {
        app.quick_notice = Some(crate::actions::Notice::new(
            "resolve cwd failed".into(),
            true,
        ));
        return;
    };
    let Some(cfg_path) = ax_config::find_config_file(cwd) else {
        app.quick_notice = Some(crate::actions::Notice::new(
            "no .ax/config.yaml found".into(),
            true,
        ));
        return;
    };
    let ax_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ax"));
    let outcomes = crate::actions::apply_lifecycle(
        pending.action,
        &pending.workspace,
        &opts.socket_path,
        &cfg_path,
        &ax_bin,
    );
    crate::actions::apply_outcomes(app, outcomes);
}

fn drain_pending_task_action(app: &mut App, opts: &RunOptions, job: &mut Option<TaskActionJob>) {
    if poll_task_action_job(app, opts, job) {
        return;
    }
    let Some(pending) = app.pending_task_action.take() else {
        return;
    };
    let opts = opts.clone();
    let (tx, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = run_pending_task_action(&pending, &opts).map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    *job = Some(TaskActionJob { receiver });
}

fn poll_task_action_job(app: &mut App, opts: &RunOptions, job: &mut Option<TaskActionJob>) -> bool {
    let Some(current) = job.as_ref() else {
        return false;
    };
    let result = match current.receiver.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Empty) => return true,
        Err(TryRecvError::Disconnected) => Err("task action worker disconnected".to_owned()),
    };
    *job = None;
    match result {
        Ok(text) => {
            app.quick_notice = Some(Notice::new(text, false));
            refresh_tasks(app, opts);
        }
        Err(e) => {
            app.quick_notice = Some(Notice::new(format!("task action failed: {e}"), true));
        }
    }
    true
}

fn drain_pending_task_create(app: &mut App, opts: &RunOptions) {
    let Some(pending) = app.pending_task_create.take() else {
        return;
    };
    let result = run_pending_task_create(&pending, opts);
    match result {
        Ok(task) => {
            app.create_task_form = None;
            app.stream = crate::stream::StreamView::Tasks;
            app.force_refresh = true;
            app.quick_notice = Some(Notice::new(
                format!(
                    "created task {} · {}",
                    crate::tasks::short_task_id(&task.id),
                    task.assignee
                ),
                false,
            ));
            refresh_tasks(app, opts);
        }
        Err(e) => {
            let text = format!("create task failed: {e}");
            if let Some(form) = app.create_task_form.as_mut() {
                form.submitting = false;
                form.error = Some(text.clone());
            }
            app.quick_notice = Some(Notice::new(text, true));
        }
    }
}

fn run_pending_task_create(
    pending: &PendingTaskCreate,
    opts: &RunOptions,
) -> Result<ax_proto::types::Task, crate::daemon::DaemonClientError> {
    let mut client = Client::connect_as(&opts.socket_path, "_cli")?;
    client.create_task(
        &pending.draft.title,
        &pending.draft.description,
        &pending.draft.assignee,
    )
}

fn drain_pending_orchestrator_message(
    app: &mut App,
    opts: &RunOptions,
    job: &mut Option<MessageSendJob>,
) {
    if poll_message_send_job(app, opts, job) {
        return;
    }
    let Some(pending) = app.pending_orchestrator_message.take() else {
        return;
    };
    let opts = opts.clone();
    let (tx, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = run_pending_orchestrator_message(&pending, &opts).map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    *job = Some(MessageSendJob { receiver });
}

fn poll_message_send_job(
    app: &mut App,
    opts: &RunOptions,
    job: &mut Option<MessageSendJob>,
) -> bool {
    let Some(current) = job.as_ref() else {
        return false;
    };
    let result = match current.receiver.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Empty) => return true,
        Err(TryRecvError::Disconnected) => Err("message send worker disconnected".to_owned()),
    };
    *job = None;
    match result {
        Ok(text) => {
            app.orchestrator_message_form = None;
            app.quick_notice = Some(Notice::new(text, false));
            app.force_refresh = true;
            refresh_messages(app, opts);
        }
        Err(e) => {
            let text = format!("send message failed: {e}");
            if let Some(form) = app.orchestrator_message_form.as_mut() {
                form.submitting = false;
                form.error = Some(text.clone());
            }
            app.quick_notice = Some(Notice::new(text, true));
        }
    }
    true
}

fn run_pending_orchestrator_message(
    pending: &PendingOrchestratorMessage,
    opts: &RunOptions,
) -> Result<String, crate::daemon::DaemonClientError> {
    let config_path = std::env::current_dir()
        .ok()
        .and_then(ax_config::find_config_file);
    let mut client = Client::connect_as(&opts.socket_path, "_cli")?;
    let response = client.send_message(
        ROOT_ORCHESTRATOR_WORKSPACE,
        &pending.message,
        config_path.as_deref(),
    )?;
    if response.message_id.is_empty() {
        Ok(format!(
            "message to {ROOT_ORCHESTRATOR_WORKSPACE} suppressed"
        ))
    } else {
        Ok(format!(
            "sent message to {ROOT_ORCHESTRATOR_WORKSPACE} · {}",
            response.message_id
        ))
    }
}

fn run_pending_task_action(
    pending: &PendingTaskAction,
    opts: &RunOptions,
) -> Result<String, crate::daemon::DaemonClientError> {
    let mut client = Client::connect_as(&opts.socket_path, "_cli")?;
    let expected_version = (pending.expected_version > 0).then_some(pending.expected_version);
    match pending.action {
        QuickActionId::TaskWake => {
            let response = client.intervene_task(
                &pending.task_id,
                "wake",
                "requested from ax watch tasks tab",
                expected_version,
            )?;
            Ok(format_intervention_notice(&response))
        }
        QuickActionId::TaskInterrupt => {
            let response = client.intervene_task(
                &pending.task_id,
                "interrupt",
                "requested from ax watch tasks tab",
                expected_version,
            )?;
            Ok(format_intervention_notice(&response))
        }
        QuickActionId::TaskRetry => {
            let response = client.intervene_task(
                &pending.task_id,
                "retry",
                "requested from ax watch tasks tab",
                expected_version,
            )?;
            Ok(format_intervention_notice(&response))
        }
        QuickActionId::TaskCancel => {
            let task = client.cancel_task(
                &pending.task_id,
                "cancelled from ax watch tasks tab",
                expected_version,
            )?;
            Ok(format!(
                "cancelled task {} · {}",
                crate::tasks::short_task_id(&task.id),
                crate::tasks::task_status_label(&task)
            ))
        }
        _ => Ok("task action ignored".to_owned()),
    }
}

fn format_intervention_notice(response: &ax_proto::responses::InterveneTaskResponse) -> String {
    let mut text = format!(
        "{} task {} · {}",
        response.action,
        crate::tasks::short_task_id(&response.task.id),
        response.status
    );
    if !response.message_id.is_empty() {
        text.push_str(" · message queued");
    }
    text
}

fn refresh(app: &mut App, opts: &RunOptions) {
    match ax_tmux::list_sessions() {
        Ok(sessions) => {
            app.sessions = sessions;
            app.clear_refresh_notice(RefreshNoticeSource::Tmux);
        }
        Err(e) => app.set_refresh_notice(
            RefreshNoticeSource::Tmux,
            format!("tmux list-sessions: {e}"),
        ),
    }

    if let Ok(mut client) = Client::connect(&opts.socket_path) {
        app.daemon_running = true;
        match client.list_workspaces() {
            Ok(workspaces) => {
                app.workspace_infos = workspaces
                    .into_iter()
                    .map(|ws| (ws.name.clone(), ws))
                    .collect();
                app.clear_refresh_notice(RefreshNoticeSource::Daemon);
            }
            Err(e) => app.set_refresh_notice(RefreshNoticeSource::Daemon, format!("daemon: {e}")),
        }
    } else {
        app.daemon_running = false;
        app.workspace_infos.clear();
    }

    // Re-read the config tree each tick so users editing ax.yaml see
    // changes take effect without restarting the TUI. Failures are
    // silent — a missing config just falls back to the name-split
    // fallback tree.
    refresh_tree(app);
    app.rebuild_agents();
    refresh_messages(app, opts);
    refresh_tasks(app, opts);
    refresh_captures(app);
    refresh_usage(app, opts);
    app.last_refresh = Some(Instant::now());
}

/// Ask the daemon for rolled-up token totals, throttled to
/// `USAGE_REFRESH_INTERVAL` so the TUI doesn't rescan every transcript
/// on each 250 ms redraw. Quiet on failure — a dropped daemon is
/// already signalled by `daemon_running = false`.
fn refresh_usage(app: &mut App, opts: &RunOptions) {
    if !app.daemon_running || app.workspace_dirs.is_empty() {
        return;
    }
    let due = app
        .last_usage_refresh
        .is_none_or(|t| t.elapsed() >= USAGE_REFRESH_INTERVAL);
    if !due {
        return;
    }
    let bindings: Vec<(String, String)> = app
        .workspace_dirs
        .iter()
        .map(|(name, dir)| (name.clone(), dir.display().to_string()))
        .collect();
    let Ok(mut client) = Client::connect(&opts.socket_path) else {
        return;
    };
    match client.usage_trends(&bindings, USAGE_WINDOW_MINUTES, USAGE_BUCKET_MINUTES) {
        Ok(trends) => {
            app.usage_trends = trends
                .into_iter()
                .map(|t| (t.workspace.clone(), t))
                .collect();
            app.clamp_token_selection();
            app.last_usage_refresh = Some(Instant::now());
            app.clear_refresh_notice(RefreshNoticeSource::Usage);
        }
        Err(e) => app.set_refresh_notice(RefreshNoticeSource::Usage, format!("usage_trends: {e}")),
    }
}

fn refresh_captures(app: &mut App) {
    // Streaming mode pins the focused workspace so the mirrored
    // pane updates every tick. Otherwise fall back to the agents-panel
    // cursor's workspace.
    let focused = app
        .streamed_workspace
        .clone()
        .or_else(|| app.selected_workspace().map(str::to_owned));
    app.captures
        .refresh(&app.sessions, focused.as_deref(), Instant::now());
    app.captures.prune(&app.sessions);
}

const MESSAGE_HISTORY_BUFFER: usize = 500;

fn refresh_messages(app: &mut App, opts: &RunOptions) {
    let path = crate::stream::history_file_path(&opts.socket_path);
    match crate::stream::read_history_snapshot(&path, MESSAGE_HISTORY_BUFFER) {
        crate::stream::SnapshotRead::Loaded(messages) => {
            app.messages = messages;
            app.messages_snapshot_error = None;
        }
        crate::stream::SnapshotRead::Missing => {
            app.messages.clear();
            app.messages_snapshot_error = None;
        }
        crate::stream::SnapshotRead::Error(message) => {
            app.messages_snapshot_error = Some(message);
        }
    }
    // Keep the cursor consistent with the live log: bump it forward
    // in follow-tail mode so new entries appear selected, and clamp
    // if the buffer shrank underneath a parked selection.
    app.reconcile_message_cursor();
}

fn refresh_tasks(app: &mut App, opts: &RunOptions) {
    let path = crate::tasks::tasks_file_path(&opts.socket_path);
    match crate::tasks::read_tasks_snapshot(&path) {
        crate::tasks::SnapshotRead::Loaded(tasks) => {
            app.tasks = tasks;
            app.task_snapshot_error = None;
        }
        crate::tasks::SnapshotRead::Missing => {
            app.tasks.clear();
            app.task_snapshot_error = None;
        }
        crate::tasks::SnapshotRead::Error(message) => {
            app.task_snapshot_error = Some(message);
        }
    }
    app.clamp_task_selection();
}

fn refresh_tree(app: &mut App) {
    let Ok(cwd) = std::env::current_dir() else {
        app.tree = None;
        app.desired.clear();
        app.reconfigure_enabled = false;
        app.workspace_dirs.clear();
        return;
    };
    let Some(cfg_path) = ax_config::find_config_file(cwd) else {
        app.tree = None;
        app.desired.clear();
        app.reconfigure_enabled = false;
        app.workspace_dirs.clear();
        return;
    };
    let tree = ax_config::Config::load_tree(&cfg_path).ok();
    // `Config::load` normalises every workspace dir to an absolute
    // path, which is exactly what the `usage_trends` handler needs to
    // derive Claude project + `CODEX_HOME` bindings.
    let flat = ax_config::Config::load(&cfg_path).ok();
    let reconfigure = flat
        .as_ref()
        .is_some_and(|cfg| cfg.experimental_mcp_team_reconfigure);
    app.reconfigure_enabled = reconfigure;
    if let Some(ref tree) = tree {
        app.desired = build_desired_set(tree, reconfigure);
    } else {
        app.desired.clear();
    }
    app.workspace_dirs = flat
        .as_ref()
        .map(|cfg| {
            cfg.workspaces
                .iter()
                .map(|(name, ws)| (name.clone(), PathBuf::from(&ws.dir)))
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    app.tree = tree;
}

fn build_desired_set(
    tree: &ax_config::ProjectNode,
    reconfigure_enabled: bool,
) -> std::collections::BTreeMap<String, bool> {
    let mut out = std::collections::BTreeMap::new();
    if !reconfigure_enabled {
        return out;
    }
    walk_desired(tree, &mut out);
    out
}

fn walk_desired(node: &ax_config::ProjectNode, out: &mut std::collections::BTreeMap<String, bool>) {
    if !(node.prefix.is_empty() && node.disable_root_orchestrator) {
        let name = if node.prefix.is_empty() {
            "orchestrator".to_owned()
        } else {
            format!("{}.orchestrator", node.prefix)
        };
        out.insert(name, true);
    }
    for ws in &node.workspaces {
        out.insert(ws.merged_name.clone(), true);
    }
    for child in &node.children {
        walk_desired(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    use ax_proto::payloads::{InterveneTaskPayload, RegisterPayload, SendMessagePayload};
    use ax_proto::responses::{InterveneTaskResponse, SendMessageResponse, StatusResponse};
    use ax_proto::types::{Task, TaskStartMode, TaskStatus};
    use ax_proto::{Envelope, MessageType, ResponsePayload};
    use chrono::Utc;
    use tempfile::TempDir;

    #[test]
    fn refresh_tasks_preserves_last_good_rows_on_malformed_snapshot() {
        let tmp = TempDir::new().unwrap();
        let opts = RunOptions {
            socket_path: tmp.path().join("daemon.sock"),
        };
        let path = crate::tasks::tasks_file_path(&opts.socket_path);
        std::fs::write(&path, serde_json::to_string(&vec![mock_task("a")]).unwrap()).unwrap();

        let mut app = App::new();
        refresh_tasks(&mut app, &opts);
        assert_eq!(app.tasks.len(), 1);
        assert!(app.task_snapshot_error.is_none());

        std::fs::write(&path, "{not json").unwrap();
        refresh_tasks(&mut app, &opts);
        assert_eq!(app.tasks.len(), 1, "last good tasks stay visible");
        assert!(app
            .task_snapshot_error
            .as_deref()
            .is_some_and(|message| message.contains("parse")));
    }

    #[test]
    fn refresh_messages_preserves_last_good_rows_on_malformed_snapshot() {
        let tmp = TempDir::new().unwrap();
        let opts = RunOptions {
            socket_path: tmp.path().join("daemon.sock"),
        };
        let path = crate::stream::history_file_path(&opts.socket_path);
        let entry = ax_daemon::HistoryEntry {
            timestamp: Utc::now(),
            from: "orch".into(),
            to: "alpha".into(),
            content: "hello".into(),
            task_id: String::new(),
        };
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();

        let mut app = App::new();
        refresh_messages(&mut app, &opts);
        assert_eq!(app.messages.len(), 1);
        assert!(app.messages_snapshot_error.is_none());

        std::fs::write(&path, "{not json}\n").unwrap();
        refresh_messages(&mut app, &opts);
        assert_eq!(app.messages.len(), 1, "last good messages stay visible");
        assert!(app
            .messages_snapshot_error
            .as_deref()
            .is_some_and(|message| message.contains("line 1")));
    }

    #[test]
    fn pending_task_action_does_not_block_tui_loop_while_daemon_responds() {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind fake daemon");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept client");
            let mut reader = BufReader::new(stream);

            let register = read_env(&mut reader);
            assert_eq!(register.r#type, MessageType::Register);
            let register_payload: RegisterPayload =
                register.decode_payload().expect("decode register");
            assert_eq!(register_payload.workspace, "_cli");
            thread::sleep(Duration::from_millis(250));
            write_response(
                reader.get_mut(),
                &register.id,
                &StatusResponse {
                    status: "ok".into(),
                },
            );

            let intervene = read_env(&mut reader);
            assert_eq!(intervene.r#type, MessageType::InterveneTask);
            let payload: InterveneTaskPayload =
                intervene.decode_payload().expect("decode intervene");
            assert_eq!(payload.id, "abcdef123456");
            assert_eq!(payload.action, "wake");
            assert_eq!(
                payload.note, "requested from ax watch tasks tab",
                "TUI wake path should use the task intervention API"
            );
            write_response(
                reader.get_mut(),
                &intervene.id,
                &InterveneTaskResponse {
                    task: mock_task(&payload.id),
                    action: payload.action,
                    status: "woken".into(),
                    message_id: "message-1".into(),
                },
            );
        });

        let opts = RunOptions { socket_path };
        let mut app = App::new();
        app.pending_task_action = Some(PendingTaskAction {
            action: QuickActionId::TaskWake,
            task_id: "abcdef123456".into(),
            expected_version: 7,
        });
        let mut job = None;

        let started = Instant::now();
        drain_pending_task_action(&mut app, &opts, &mut job);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "starting the daemon request should not stall the render/input loop"
        );
        assert!(app.pending_task_action.is_none());
        assert!(
            job.is_some(),
            "task action should be running in the background"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while job.is_some() && Instant::now() < deadline {
            drain_pending_task_action(&mut app, &opts, &mut job);
            thread::sleep(Duration::from_millis(10));
        }

        assert!(job.is_none(), "background task action should complete");
        let notice = app.quick_notice.as_ref().expect("completion notice");
        assert!(notice.text.contains("wake task abcdef12"));
        assert!(notice.text.contains("message queued"));
        server.join().expect("server thread");
    }

    #[test]
    fn pending_orchestrator_message_uses_plain_send_message_path() {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("daemon.sock");
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
            assert_eq!(payload.to, ROOT_ORCHESTRATOR_WORKSPACE);
            assert_eq!(payload.message, "check queue health");

            write_response(
                reader.get_mut(),
                &send.id,
                &SendMessageResponse {
                    message_id: "msg-1".into(),
                    status: "sent".into(),
                },
            );
        });

        let opts = RunOptions { socket_path };
        let pending = PendingOrchestratorMessage {
            message: "check queue health".into(),
        };

        let notice = run_pending_orchestrator_message(&pending, &opts).expect("send message");
        assert!(notice.contains("sent message to orchestrator"));
        assert!(notice.contains("msg-1"));
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

    fn mock_task(id: &str) -> Task {
        let now = Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            assignee: "alpha".into(),
            created_by: "orch".into(),
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
