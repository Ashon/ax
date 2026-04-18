//! Tool-level tests for the agents, `interrupt_agent` / `send_keys`,
//! and `team_reconfigure` groups. These exercise the happy and error
//! boundaries we can check without a live tmux — agent lifecycle
//! errors surface through the daemon's `handle_agent_lifecycle`
//! unknown-name branch, and team state returns the initial empty
//! overlay state when no experimental flag is set.

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
async fn list_agents_returns_configured_agents_from_yaml() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch", &cfg).await;

    let listed = server
        .list_agents(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("list_agents");
    let body: serde_json::Value =
        serde_json::from_str(&call_text(&listed)).expect("decode list_agents body");
    assert_eq!(body["project"], "demo");
    assert_eq!(body["agent_count"], 2);
    let agents = body["agents"].as_array().expect("agents array");
    assert_eq!(agents[0]["name"], "alpha");
    assert_eq!(agents[0]["launch_mode"], "runtime");
    assert_eq!(agents[0]["runtime"], "codex");
    assert_eq!(agents[1]["name"], "beta");
    assert_eq!(agents[1]["launch_mode"], "manual");

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn list_agents_query_filter_matches_by_description() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch", &cfg).await;

    let listed = server
        .list_agents(Parameters(
            serde_json::from_value(serde_json::json!({ "query": "other" })).expect("decode"),
        ))
        .await
        .expect("list_agents");
    let body: serde_json::Value = serde_json::from_str(&call_text(&listed)).expect("decode");
    assert_eq!(body["agent_count"], 1);
    assert_eq!(body["agents"][0]["name"], "beta");

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn start_agent_errors_when_name_unknown() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch", &cfg).await;

    let err = server
        .start_agent(Parameters(
            serde_json::from_value(serde_json::json!({ "name": "ghost" })).expect("decode"),
        ))
        .await
        .expect_err("start_agent must error on unknown name");
    assert!(
        err.to_string().to_lowercase().contains("ghost")
            || err.to_string().to_lowercase().contains("unknown")
            || err.to_string().to_lowercase().contains("not found"),
        "unexpected error body: {err}"
    );

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn interrupt_agent_reports_session_missing() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch", &cfg).await;

    let err = server
        .interrupt_agent(Parameters(
            serde_json::from_value(serde_json::json!({ "name": "alpha" })).expect("decode"),
        ))
        .await
        .expect_err("no tmux session yet");
    assert!(err.to_string().contains("not running"), "body: {err}");

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn send_keys_rejects_empty_sequence() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch", &cfg).await;

    let err = server
        .send_keys(Parameters(
            serde_json::from_value(serde_json::json!({
                "workspace": "alpha",
                "keys": [],
            }))
            .expect("decode"),
        ))
        .await
        .expect_err("send_keys rejects empty list");
    assert!(err.to_string().contains("keys"), "body: {err}");

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn get_team_state_returns_initial_snapshot() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch", &cfg).await;

    let resp = server.get_team_state().await.expect("get_team_state");
    let body: serde_json::Value =
        serde_json::from_str(&call_text(&resp)).expect("decode team state");
    assert_eq!(body["feature_enabled"], false);
    assert!(body["base_config_path"].as_str().is_some());

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn dry_run_team_reconfigure_requires_changes() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = write_config(tmp.path());
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch", &cfg).await;

    let err = server
        .dry_run_team_reconfigure(Parameters(
            serde_json::from_value(serde_json::json!({ "changes": [] })).expect("decode"),
        ))
        .await
        .expect_err("empty changes rejects");
    assert!(err.to_string().contains("changes"), "body: {err}");

    server.daemon().close().await;
    handle.shutdown().await;
}
