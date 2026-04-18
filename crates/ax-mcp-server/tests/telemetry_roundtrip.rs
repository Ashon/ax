//! Exercise the full MCP tool-call path — duplex transport, `#[tool_handler]`
//! dispatch, then telemetry sink — so a regression that drops the logging
//! hook from `Server::call_tool` fails fast.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use rmcp::{ClientHandler, ServiceExt};
use tempfile::TempDir;

use ax_daemon::{Daemon, DaemonHandle};
use ax_mcp_server::{DaemonClient, Server, TelemetryEvent, TelemetrySink};

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

#[derive(Default, Clone)]
struct StubClient;

impl ClientHandler for StubClient {
    fn get_info(&self) -> rmcp::model::ClientInfo {
        rmcp::model::ClientInfo::default()
    }
}

#[tokio::test]
async fn call_tool_via_mcp_transport_records_telemetry() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();
    let handle = spawn_daemon(&state_dir).await;

    let daemon = DaemonClient::builder(handle.socket_path(), "alpha")
        .dir("/tmp/alpha")
        .connect()
        .await
        .expect("daemon client connects");

    let telemetry_path = state_dir.join("telemetry").join("tool_calls.jsonl");
    let server = Server::new(daemon).with_telemetry(TelemetrySink::new(&telemetry_path));

    let (server_transport, client_transport) = tokio::io::duplex(8 * 1024);
    let server_handle =
        tokio::spawn(async move { server.serve(server_transport).await.unwrap().waiting().await });
    let client = StubClient
        .serve(client_transport)
        .await
        .expect("client handshake");

    client
        .call_tool(CallToolRequestParams::new("list_shared_values"))
        .await
        .expect("list_shared_values succeeds");

    drop(client);
    let _ = server_handle.await;

    let body = std::fs::read_to_string(&telemetry_path).expect("telemetry file written");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 1, "one call, one record: {body}");
    let event: TelemetryEvent = serde_json::from_str(lines[0]).expect("parse event");
    assert_eq!(event.tool, "list_shared_values");
    assert_eq!(event.workspace, "alpha");
    assert!(event.ok, "call should succeed against in-process daemon");

    handle.shutdown().await;
}

#[tokio::test]
async fn failing_tool_records_error_kind() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();
    let handle = spawn_daemon(&state_dir).await;

    let daemon = DaemonClient::builder(handle.socket_path(), "alpha")
        .dir("/tmp/alpha")
        .connect()
        .await
        .expect("daemon client connects");

    let telemetry_path = state_dir.join("telemetry").join("tool_calls.jsonl");
    let server = Server::new(daemon).with_telemetry(TelemetrySink::new(&telemetry_path));

    let (server_transport, client_transport) = tokio::io::duplex(8 * 1024);
    let server_handle =
        tokio::spawn(async move { server.serve(server_transport).await.unwrap().waiting().await });
    let client = StubClient
        .serve(client_transport)
        .await
        .expect("client handshake");

    // Invalid params: get_shared_value requires a `key`.
    let _ = client
        .call_tool(CallToolRequestParams::new("get_shared_value"))
        .await;

    drop(client);
    let _ = server_handle.await;

    let body = std::fs::read_to_string(&telemetry_path).expect("telemetry file written");
    let last = body.lines().last().expect("at least one event");
    let event: TelemetryEvent = serde_json::from_str(last).expect("parse event");
    assert_eq!(event.tool, "get_shared_value");
    assert!(!event.ok);
    assert!(!event.err_kind.is_empty(), "expected err_kind on failure");

    handle.shutdown().await;
}
