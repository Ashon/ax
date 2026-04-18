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
