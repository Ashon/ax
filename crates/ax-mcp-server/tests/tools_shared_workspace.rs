//! Tool-level tests for the shared + workspace group. Invokes the
//! typed tool methods on the `Server` struct directly (without going
//! through an MCP transport) so we can assert on the returned
//! `CallToolResult` content without having to ferry JSON-RPC frames
//! over stdio. The in-process `ax-daemon` provides the real envelope
//! plumbing behind the tool handlers.

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

fn call_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|content| content.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn connect_server(socket: &Path, workspace: &str) -> Server {
    let daemon = DaemonClient::builder(socket, workspace)
        .connect()
        .await
        .expect("daemon client connects");
    Server::new(daemon)
}

#[tokio::test]
async fn shared_values_set_get_list_roundtrip() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();
    let handle = spawn_daemon(&state_dir).await;
    let server = connect_server(handle.socket_path(), "orch").await;

    // set_shared_value
    let set = server
        .set_shared_value(Parameters(
            serde_json::from_value(serde_json::json!({
                "key": "endpoint",
                "value": "https://api.example.com",
            }))
            .expect("decode request"),
        ))
        .await
        .expect("set succeeds");
    assert!(call_text(&set).contains("\"ok\":true"));

    // get_shared_value
    let got = server
        .get_shared_value(Parameters(
            serde_json::from_value(serde_json::json!({
                "key": "endpoint",
            }))
            .expect("decode request"),
        ))
        .await
        .expect("get succeeds");
    let body = call_text(&got);
    assert!(body.contains("https://api.example.com"));
    assert!(body.contains("\"found\": true"));

    // list_shared_values
    let listed = server.list_shared_values().await.expect("list succeeds");
    let body = call_text(&listed);
    assert!(body.contains("endpoint"));
    assert!(body.contains("https://api.example.com"));

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn workspace_status_and_list_tools_reflect_registry() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let _worker = connect_server(handle.socket_path(), "worker").await;

    // set_status on the orchestrator
    let _ = orch
        .set_status(Parameters(
            serde_json::from_value(serde_json::json!({
                "status": "coordinating",
            }))
            .expect("decode request"),
        ))
        .await
        .expect("set_status succeeds");

    // list_workspaces sees both registrations
    let listed = orch.list_workspaces().await.expect("list succeeds");
    let body = call_text(&listed);
    assert!(body.contains("\"count\": 2"), "body: {body}");
    assert!(body.contains("\"name\": \"orch\""));
    assert!(body.contains("\"name\": \"worker\""));
    assert!(body.contains("coordinating"));

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn list_workspaces_exposes_last_activity_at_per_peer() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let _worker = connect_server(handle.socket_path(), "worker").await;

    // Any MCP call on orch bumps its last_activity_at. We verify the
    // field is present and non-null for both peers so orchestrators
    // can answer "how long has B been quiet" without polling
    // registry-internal timestamps.
    let _ = orch
        .set_status(Parameters(
            serde_json::from_value(serde_json::json!({ "status": "driving" }))
                .expect("decode"),
        ))
        .await
        .expect("set_status");

    let listed = orch.list_workspaces().await.expect("list succeeds");
    let body: serde_json::Value =
        serde_json::from_str(&call_text(&listed)).expect("decode list_workspaces");
    let workspaces = body["workspaces"].as_array().expect("workspaces array");
    assert_eq!(workspaces.len(), 2, "body: {body}");
    for ws in workspaces {
        let ts = ws["last_activity_at"]
            .as_str()
            .unwrap_or_else(|| panic!("last_activity_at missing on {ws}"));
        // RFC3339 starts with a 4-digit year; any reasonable
        // timestamp fits that shape. This guards against a future
        // regression where the field is present but encoded as a
        // different primitive (e.g. epoch seconds as an int).
        assert!(ts.len() >= 10, "suspicious timestamp shape: {ts}");
        assert!(ts.contains('T'), "expected RFC3339 timestamp, got {ts}");
    }

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn get_shared_value_returns_not_found_cleanly() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch").await;

    let got = server
        .get_shared_value(Parameters(
            serde_json::from_value(serde_json::json!({
                "key": "missing",
            }))
            .expect("decode request"),
        ))
        .await
        .expect("get succeeds");
    let body = call_text(&got);
    assert!(body.contains("\"found\": false"), "body: {body}");

    server.daemon().close().await;
    handle.shutdown().await;
}
