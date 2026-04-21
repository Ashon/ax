//! Tool-level tests for the tasks group. Exercises the full lifecycle
//! (create → update → get → list) plus `list_workspace_tasks` with both
//! views, cancel/remove cleanup, intervene-retry dispatch, and the
//! validation boundaries (bad status / priority / view).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use tempfile::TempDir;

use ax_daemon::{Daemon, DaemonHandle};
use ax_mcp_server::{DaemonClient, Server};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

async fn spawn_daemon(state_dir: &Path) -> DaemonHandle {
    let socket_path = state_dir.join("daemon.sock");
    let daemon = Daemon::new(socket_path)
        .with_state_dir(state_dir)
        .expect("with_state_dir");
    let handle = daemon.bind().await.expect("bind");
    for _ in 0..50 {
        if handle.socket_path().exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle
}

fn call_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|content| content.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn call_json<T: DeserializeOwned>(result: &CallToolResult) -> T {
    serde_json::from_str(&call_text(result)).expect("decode tool body as JSON")
}

async fn connect_server(socket: &Path, workspace: &str) -> Server {
    let daemon = DaemonClient::builder(socket, workspace)
        .connect()
        .await
        .expect("daemon client connects");
    Server::new(daemon)
}

#[tokio::test]
async fn create_get_update_and_list_cycle() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();
    let handle = spawn_daemon(&state_dir).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "fix flaky test",
                "description": "investigate the retry logic",
                "assignee": "worker",
                "priority": "high",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task: serde_json::Value = call_json(&created);
    let task_id = task["id"].as_str().expect("task id").to_owned();
    assert_eq!(task["assignee"], "worker");
    assert_eq!(task["status"], "pending");

    let fetched = orch
        .get_task(Parameters(
            serde_json::from_value(serde_json::json!({ "id": task_id.clone() })).expect("decode"),
        ))
        .await
        .expect("get");
    let fetched_body: serde_json::Value = call_json(&fetched);
    assert_eq!(fetched_body["id"], task_id);

    let updated = worker
        .update_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id.clone(),
                "status": "in_progress",
                "log": "starting",
            }))
            .expect("decode"),
        ))
        .await
        .expect("update");
    let updated_body: serde_json::Value = call_json(&updated);
    assert_eq!(updated_body["status"], "in_progress");

    let listed = orch
        .list_tasks(Parameters(
            serde_json::from_value(serde_json::json!({ "assignee": "worker" })).expect("decode"),
        ))
        .await
        .expect("list");
    let body: serde_json::Value = call_json(&listed);
    assert_eq!(body["count"], 1);

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn list_tasks_returns_no_tasks_friendly_when_empty() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let listed = orch
        .list_tasks(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("list");
    assert!(call_text(&listed).contains("No tasks found."));
    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn list_workspace_tasks_both_view_aggregates_assigned_and_created() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    // orch creates a task for worker (worker.assigned, orch.created).
    let _: CallToolResult = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "t1",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    // worker creates a task for orch (worker.created, orch.assigned).
    let _: CallToolResult = worker
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "t2",
                "assignee": "orch",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");

    let both = orch
        .list_workspace_tasks(Parameters(
            serde_json::from_value(serde_json::json!({ "workspace": "worker" })).expect("decode"),
        ))
        .await
        .expect("list");
    let body: serde_json::Value = call_json(&both);
    assert_eq!(body["view"], "both");
    assert_eq!(body["unique_task_count"], 2);
    assert_eq!(body["assigned"]["count"], 1);
    assert_eq!(body["created"]["count"], 1);

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn cancel_and_remove_task_flow() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "junk",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task: serde_json::Value = call_json(&created);
    let task_id = task["id"].as_str().expect("id").to_owned();

    let cancelled = orch
        .cancel_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id.clone(),
                "reason": "scope cut",
            }))
            .expect("decode"),
        ))
        .await
        .expect("cancel");
    let cancelled_body: serde_json::Value = call_json(&cancelled);
    assert_eq!(cancelled_body["status"], "cancelled");

    let removed = orch
        .remove_task(Parameters(
            serde_json::from_value(serde_json::json!({ "id": task_id.clone() })).expect("decode"),
        ))
        .await
        .expect("remove");
    let removed_body: serde_json::Value = call_json(&removed);
    assert!(removed_body["removed_at"].is_string());

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn update_task_rejects_unknown_status_value() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "x",
                "assignee": "orch",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let err = orch
        .update_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "status": "nope",
            }))
            .expect("decode"),
        ))
        .await
        .expect_err("bad status rejects");
    assert!(err.to_string().contains("invalid status"));

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn start_task_validation_error_returns_tool_error_result() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;

    let result = orch
        .start_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "bad dispatch body",
                "assignee": "worker",
                "message": "Task ID: 11111111-2222-3333-4444-555555555555 do work",
            }))
            .expect("decode"),
        ))
        .await
        .expect("daemon validation should be a tool error result");
    assert_eq!(result.is_error, Some(true));
    let body = call_text(&result);
    assert!(body.contains("Task ID"), "body: {body}");
    assert!(body.contains("start_task injects"), "body: {body}");

    let listed = orch
        .list_tasks(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("list");
    assert!(call_text(&listed).contains("No tasks found."));

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn list_workspace_tasks_rejects_invalid_view() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;

    let err = orch
        .list_workspace_tasks(Parameters(
            serde_json::from_value(serde_json::json!({
                "workspace": "worker",
                "view": "random",
            }))
            .expect("decode"),
        ))
        .await
        .expect_err("invalid view rejects");
    assert!(err.to_string().contains("invalid view"));

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn start_task_queues_dispatch_message() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let _worker = connect_server(handle.socket_path(), "worker").await;

    let resp = orch
        .start_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "go",
                "assignee": "worker",
                "message": "please do the thing",
            }))
            .expect("decode"),
        ))
        .await
        .expect("start");
    let body: serde_json::Value = call_json(&resp);
    assert_eq!(body["task"]["assignee"], "worker");
    assert_eq!(body["dispatch"]["status"], "queued");
    assert!(body["dispatch"]["message_id"].as_str().is_some());

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn task_completion_notifies_creator_inbox() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "do the thing",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    // Worker transitions the task to completed, satisfying the contract
    // (marker + confirm).
    worker
        .update_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "status": "completed",
                "result": "did the thing. remaining owned dirty files=<none>",
                "confirm": true,
            }))
            .expect("decode"),
        ))
        .await
        .expect("update to completed");

    let inbox = orch
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("read");
    let body = call_text(&inbox);
    assert!(body.contains("1 message(s):"), "body: {body}");
    assert!(body.contains("From: worker"), "body: {body}");
    assert!(body.contains("[task-completed]"), "body: {body}");
    assert!(body.contains(&task_id), "body: {body}");

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn completion_contract_rejection_enqueues_reminder_in_worker_inbox() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "build it",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    // Worker tries to complete without the leftover-scope marker.
    let err = worker
        .update_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "status": "completed",
                "result": "looks good to me",
                "confirm": true,
            }))
            .expect("decode"),
        ))
        .await
        .expect_err("missing marker must reject");
    assert!(
        err.to_string().contains("leftover-scope"),
        "body: {err}"
    );

    // The worker's inbox should now have a durable reminder so next
    // poll surfaces the contract requirement again.
    let inbox = worker
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("read");
    let body = call_text(&inbox);
    assert!(body.contains("[task-completion-rejected]"), "body: {body}");
    assert!(body.contains(&task_id), "body: {body}");
    assert!(body.contains("remaining owned dirty files"), "body: {body}");

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_failed_transitions_and_notifies_creator() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "broken-thing",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let resp = worker
        .report_task_failed(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "reason": "upstream API returned 503 repeatedly",
            }))
            .expect("decode"),
        ))
        .await
        .expect("fail succeeds");
    let task: serde_json::Value = call_json(&resp);
    assert_eq!(task["status"], "failed");

    let inbox = orch
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("read");
    let body = call_text(&inbox);
    assert!(body.contains("[task-failed]"), "body: {body}");
    assert!(body.contains(&task_id), "body: {body}");
    assert!(body.contains("503"), "body: {body}");

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_failed_rejects_empty_reason() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let err = worker
        .report_task_failed(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": "t-anything",
                "reason": "   ",
            }))
            .expect("decode"),
        ))
        .await
        .expect_err("empty reason must reject");
    assert!(err.to_string().contains("reason"), "body: {err}");

    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_blocked_transitions_and_notifies_creator() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "gated",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let resp = worker
        .report_task_blocked(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "reason": "missing auth credential",
            }))
            .expect("decode"),
        ))
        .await
        .expect("block succeeds");
    let task: serde_json::Value = call_json(&resp);
    assert_eq!(task["status"], "blocked");

    // Creator receives a terminal-status notification (piggy-backing
    // on the iteration-1 completion-push infrastructure).
    let inbox = orch
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("read");
    let body = call_text(&inbox);
    assert!(body.contains("[task-blocked]"), "body: {body}");
    assert!(body.contains(&task_id), "body: {body}");

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_blocked_sends_help_request_to_named_peer() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;
    let helper = connect_server(handle.socket_path(), "helper").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "needs-help",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    worker
        .report_task_blocked(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "reason": "need the latest API spec",
                "needs_help_from": "helper",
            }))
            .expect("decode"),
        ))
        .await
        .expect("block succeeds");

    let helper_inbox = helper
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("read");
    let body = call_text(&helper_inbox);
    assert!(body.contains("[task-blocked-help]"), "body: {body}");
    assert!(body.contains("need the latest API spec"), "body: {body}");
    assert!(body.contains("From: worker"), "body: {body}");

    orch.daemon().close().await;
    worker.daemon().close().await;
    helper.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_blocked_rejects_empty_reason() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let err = worker
        .report_task_blocked(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": "t-anything",
                "reason": "   ",
            }))
            .expect("decode"),
        ))
        .await
        .expect_err("empty reason must reject before hitting daemon");
    assert!(err.to_string().contains("reason"), "body: {err}");

    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_progress_promotes_pending_to_in_progress() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "heartbeat me",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let resp = worker
        .report_task_progress(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "note": "reading source files",
            }))
            .expect("decode"),
        ))
        .await
        .expect("report succeeds");
    let task: serde_json::Value = call_json(&resp);
    assert_eq!(task["status"], "in_progress");
    // Log entry should contain the note we sent.
    let logs = task["logs"].as_array().expect("logs array");
    let note_found = logs.iter().any(|entry| {
        entry["message"]
            .as_str()
            .or_else(|| entry["note"].as_str())
            .or_else(|| entry.as_str())
            .map(|s| s.contains("reading source files"))
            .unwrap_or(false)
    });
    assert!(note_found, "logs: {logs:?}");

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_progress_rejects_empty_note() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let err = worker
        .report_task_progress(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": "t-anything",
                "note": "   ",
            }))
            .expect("decode"),
        ))
        .await
        .expect_err("empty note must reject before hitting daemon");
    assert!(err.to_string().contains("note"), "body: {err}");

    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_completion_marks_completed_when_clean() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "deliver X",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let resp = worker
        .report_task_completion(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "summary": "shipped feature X and added tests",
                "dirty_files": [],
            }))
            .expect("decode"),
        ))
        .await
        .expect("report succeeds");
    let task: serde_json::Value = call_json(&resp);
    assert_eq!(task["status"], "completed");
    // The canonical marker must be baked into the stored result so
    // other tools that parse it (silent-exit reconciler, history
    // views) see the same contract-compliant string.
    assert!(
        task["result"]
            .as_str()
            .unwrap_or_default()
            .contains("remaining owned dirty files=<none>"),
        "task: {task}"
    );

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_completion_rejects_dirty_files_without_residual_scope() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "partial",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let err = worker
        .report_task_completion(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "summary": "partial work",
                "dirty_files": ["src/foo.rs"],
                // residual_scope intentionally omitted
            }))
            .expect("decode"),
        ))
        .await
        .expect_err("dirty files without residual scope must fail");
    assert!(
        err.to_string().contains("residual_scope"),
        "body: {err}"
    );

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn report_task_completion_packages_residual_scope_into_marker() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "bigger",
                "assignee": "worker",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let resp = worker
        .report_task_completion(Parameters(
            serde_json::from_value(serde_json::json!({
                "id": task_id,
                "summary": "shipped phase 1",
                "dirty_files": ["src/foo.rs", "src/bar.rs"],
                "residual_scope": "phase 2 integration pending",
            }))
            .expect("decode"),
        ))
        .await
        .expect("report succeeds");
    let task: serde_json::Value = call_json(&resp);
    let result_str = task["result"].as_str().unwrap_or_default();
    assert!(
        result_str.contains("remaining owned dirty files=src/foo.rs, src/bar.rs"),
        "result: {result_str}"
    );
    assert!(
        result_str.contains("residual scope=phase 2 integration pending"),
        "result: {result_str}"
    );

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn self_assigned_task_completion_does_not_notify() {
    // When creator == assignee (orch working on its own task), we
    // should not enqueue a notification to orch's own inbox.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;

    let created = orch
        .create_task(Parameters(
            serde_json::from_value(serde_json::json!({
                "title": "self task",
                "assignee": "orch",
            }))
            .expect("decode"),
        ))
        .await
        .expect("create");
    let task_id = call_json::<serde_json::Value>(&created)["id"]
        .as_str()
        .expect("id")
        .to_owned();

    orch.update_task(Parameters(
        serde_json::from_value(serde_json::json!({
            "id": task_id,
            "status": "completed",
            "result": "done. remaining owned dirty files=<none>",
            "confirm": true,
        }))
        .expect("decode"),
    ))
    .await
    .expect("update");

    let inbox = orch
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("read");
    assert!(
        call_text(&inbox).contains("No pending messages."),
        "body: {}",
        call_text(&inbox)
    );

    orch.daemon().close().await;
    handle.shutdown().await;
}
