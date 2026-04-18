//! Tool-level tests for `get_usage_trends`, `inspect_agent`, and
//! `request`. These cover the error and suppression paths plus the
//! active-registry → cwd lookup for usage, since a live reply loop
//! requires a worker workspace and is out of scope for an e2e test.

use std::path::{Path, PathBuf};
use std::time::Duration;

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

async fn connect_server(socket: &Path, workspace: &str, config_path: &Path) -> Server {
    let daemon = DaemonClient::builder(socket, workspace)
        .config_path(config_path.display().to_string())
        .dir(format!("/tmp/{workspace}"))
        .connect()
        .await
        .expect("daemon client connects");
    Server::new(daemon).with_config_path(config_path.to_path_buf())
}

fn write_config(dir: &Path) -> PathBuf {
    let ax_dir = dir.join(".ax");
    std::fs::create_dir_all(&ax_dir).expect("mkdir .ax");
    let path = ax_dir.join("config.yaml");
    std::fs::write(
        &path,
        concat!(
            "project: demo\n",
            "disable_root_orchestrator: true\n",
            "workspaces:\n",
            "  alpha:\n",
            "    dir: /tmp/alpha\n",
            "    description: cli workspace\n",
            "    runtime: codex\n",
            "  beta:\n",
            "    dir: /tmp/beta\n",
            "    description: other workspace\n",
            "    agent: none\n",
            "  worker:\n",
            "    dir: /tmp/worker\n",
            "    description: reply target\n",
            "    agent: none\n",
        ),
    )
    .expect("write config");
    path
}

fn call_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|content| content.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn get_usage_trends_lists_active_workspaces_with_defaults() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch", &cfg).await;
    let _alpha = connect_server(handle.socket_path(), "alpha", &cfg).await;

    let resp = orch
        .get_usage_trends(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("usage trends");
    let body: serde_json::Value =
        serde_json::from_str(&call_text(&resp)).expect("decode trends body");
    assert_eq!(body["since_minutes"], 180);
    assert_eq!(body["bucket_minutes"], 5);
    assert!(body["trends"].is_array(), "body: {body}");

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn get_usage_trends_falls_back_to_config_for_inactive_workspace() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch", &cfg).await;

    // `beta` is configured but no workspace has registered.
    let resp = orch
        .get_usage_trends(Parameters(
            serde_json::from_value(serde_json::json!({ "workspace": "beta" })).expect("decode"),
        ))
        .await
        .expect("usage trends");
    let body: serde_json::Value = serde_json::from_str(&call_text(&resp)).expect("decode");
    assert_eq!(body["workspace"], "beta");

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn get_usage_trends_errors_for_unknown_workspace() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch", &cfg).await;

    let err = orch
        .get_usage_trends(Parameters(
            serde_json::from_value(serde_json::json!({ "workspace": "ghost" })).expect("decode"),
        ))
        .await
        .expect_err("unknown workspace rejects");
    assert!(
        err.to_string().to_lowercase().contains("ghost"),
        "body: {err}"
    );

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn inspect_agent_rejects_unknown_name() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch", &cfg).await;

    let err = orch
        .inspect_agent(Parameters(
            serde_json::from_value(serde_json::json!({ "name": "ghost" })).expect("decode"),
        ))
        .await
        .expect_err("unknown agent rejects");
    assert!(err.to_string().contains("ghost"), "body: {err}");

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn request_message_times_out_when_reply_never_arrives() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch", &cfg).await;
    let _worker = connect_server(handle.socket_path(), "worker", &cfg).await;

    // timeout=1 makes the polling loop give up after the first tick.
    let err = orch
        .request_message(Parameters(
            serde_json::from_value(serde_json::json!({
                "to": "worker",
                "message": "please review PR",
                "timeout": 1,
            }))
            .expect("decode"),
        ))
        .await
        .expect_err("no reply comes back");
    assert!(
        err.to_string().to_lowercase().contains("timeout"),
        "body: {err}"
    );

    orch.daemon().close().await;
    handle.shutdown().await;
}
