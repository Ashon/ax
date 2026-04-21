//! Tool-level tests for the memory + messages group. Calls the
//! typed tool methods on the `Server` struct directly against an
//! in-process `ax-daemon`, checking that scope aliases normalise to
//! the daemon's stored form and that the message helpers produce
//! the expected human-friendly output shape.

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
async fn remember_and_recall_default_to_workspace_scope() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();
    let handle = spawn_daemon(&state_dir).await;
    let server = connect_server(handle.socket_path(), "orch").await;

    let remembered = server
        .remember_memory(Parameters(
            serde_json::from_value(serde_json::json!({
                "content": "api-key rotated on release branch",
                "tags": ["ops", "security"],
            }))
            .expect("decode request"),
        ))
        .await
        .expect("remember succeeds");
    let body = call_text(&remembered);
    assert!(body.contains("api-key"));
    assert!(body.contains("\"scope\": \"workspace:orch\""));

    // recall with no scope argument defaults to [global, project, workspace];
    // project resolution will fail without a config path — verify that
    // passing an explicit workspace scope also returns the record.
    let listed = server
        .list_memories(Parameters(
            serde_json::from_value(serde_json::json!({
                "scopes": ["workspace"],
            }))
            .expect("decode request"),
        ))
        .await
        .expect("list succeeds");
    let body = call_text(&listed);
    assert!(body.contains("workspace:orch"));
    assert!(body.contains("api-key"));

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn supersede_memory_requires_supersedes_ids() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch").await;

    let err = server
        .supersede_memory(Parameters(
            serde_json::from_value(serde_json::json!({
                "content": "new",
                "supersedes_ids": [],
            }))
            .expect("decode request"),
        ))
        .await
        .expect_err("supersede should reject empty list");
    assert!(err.to_string().contains("supersedes_ids"));

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn memory_scope_rejects_unknown_alias() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch").await;

    let err = server
        .remember_memory(Parameters(
            serde_json::from_value(serde_json::json!({
                "content": "x",
                "scope": "nonsense",
            }))
            .expect("decode request"),
        ))
        .await
        .expect_err("unknown scope rejects");
    assert!(err.to_string().contains("invalid memory scope"));

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn send_and_read_messages_roundtrip() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    let sent = orch
        .send_message(Parameters(
            serde_json::from_value(serde_json::json!({
                "to": "worker",
                "message": "please review PR",
            }))
            .expect("decode request"),
        ))
        .await
        .expect("send succeeds");
    let body = call_text(&sent);
    assert!(body.starts_with("Message sent to \"worker\" (id:"));

    let read = worker
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode request"),
        ))
        .await
        .expect("read succeeds");
    let body = call_text(&read);
    assert!(body.contains("1 message(s):"), "body: {body}");
    assert!(body.contains("From: orch"));
    assert!(body.contains("please review PR"));

    // Second read on the empty inbox returns the friendly string.
    let empty = worker
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode request"),
        ))
        .await
        .expect("read succeeds");
    assert!(call_text(&empty).contains("No pending messages."));

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn broadcast_reports_recipient_names() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let _a = connect_server(handle.socket_path(), "a").await;
    let _b = connect_server(handle.socket_path(), "b").await;

    let cast = orch
        .broadcast_message(Parameters(
            serde_json::from_value(serde_json::json!({ "message": "rollout today" }))
                .expect("decode request"),
        ))
        .await
        .expect("broadcast succeeds");
    let body = call_text(&cast);
    assert!(
        body.contains("Broadcast sent to 2 workspace(s)"),
        "body: {body}"
    );
    assert!(body.contains('a'));
    assert!(body.contains('b'));

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn broadcast_does_not_enqueue_for_sender() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let _a = connect_server(handle.socket_path(), "a").await;

    orch.broadcast_message(Parameters(
        serde_json::from_value(serde_json::json!({ "message": "rollout today" }))
            .expect("decode request"),
    ))
    .await
    .expect("broadcast succeeds");

    let own_inbox = orch
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("read succeeds");
    assert!(
        call_text(&own_inbox).contains("No pending messages."),
        "body: {}",
        call_text(&own_inbox)
    );

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn broadcast_delivers_identical_content_to_every_recipient() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let a = connect_server(handle.socket_path(), "a").await;
    let b = connect_server(handle.socket_path(), "b").await;

    orch.broadcast_message(Parameters(
        serde_json::from_value(serde_json::json!({ "message": "standup in 5" }))
            .expect("decode request"),
    ))
    .await
    .expect("broadcast succeeds");

    for reader in [&a, &b] {
        let body = call_text(
            &reader
                .read_messages(Parameters(
                    serde_json::from_value(serde_json::json!({})).expect("decode"),
                ))
                .await
                .expect("read succeeds"),
        );
        assert!(body.contains("1 message(s):"), "body: {body}");
        assert!(body.contains("From: orch"), "body: {body}");
        assert!(body.contains("standup in 5"), "body: {body}");
    }

    orch.daemon().close().await;
    a.daemon().close().await;
    b.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn broadcast_returns_no_recipients_message_when_alone() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let server = connect_server(handle.socket_path(), "orch").await;

    let cast = server
        .broadcast_message(Parameters(
            serde_json::from_value(serde_json::json!({ "message": "hello" }))
                .expect("decode request"),
        ))
        .await
        .expect("broadcast succeeds");
    assert!(call_text(&cast).contains("No other workspaces"));

    server.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn send_message_rejects_empty_recipient() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;

    let err = orch
        .send_message(Parameters(
            serde_json::from_value(serde_json::json!({
                "to": "   ",
                "message": "hi",
            }))
            .expect("decode request"),
        ))
        .await
        .expect_err("empty recipient must be rejected");
    assert!(
        err.to_string().contains("missing recipient"),
        "body: {err}"
    );

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn send_message_rejects_self_recipient() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;

    let err = orch
        .send_message(Parameters(
            serde_json::from_value(serde_json::json!({
                "to": "orch",
                "message": "talking to myself",
            }))
            .expect("decode request"),
        ))
        .await
        .expect_err("self-addressed must be rejected");
    assert!(err.to_string().contains("cannot send"), "body: {err}");

    orch.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn read_messages_respects_limit_parameter() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let orch = connect_server(handle.socket_path(), "orch").await;
    let worker = connect_server(handle.socket_path(), "worker").await;

    for idx in 0..3 {
        orch.send_message(Parameters(
            serde_json::from_value(serde_json::json!({
                "to": "worker",
                "message": format!("msg-{idx}"),
            }))
            .expect("decode request"),
        ))
        .await
        .expect("send succeeds");
    }

    let read = worker
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({ "limit": 2 })).expect("decode"),
        ))
        .await
        .expect("read succeeds");
    let body = call_text(&read);
    assert!(body.contains("2 message(s):"), "body: {body}");

    // Remaining message is still available on the next read.
    let rest = worker
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("read succeeds");
    assert!(call_text(&rest).contains("1 message(s):"));

    orch.daemon().close().await;
    worker.daemon().close().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn read_messages_filters_by_sender() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let alice = connect_server(handle.socket_path(), "alice").await;
    let bob = connect_server(handle.socket_path(), "bob").await;
    let carol = connect_server(handle.socket_path(), "carol").await;

    alice
        .send_message(Parameters(
            serde_json::from_value(serde_json::json!({
                "to": "carol",
                "message": "from alice",
            }))
            .expect("decode request"),
        ))
        .await
        .expect("alice sends");
    bob.send_message(Parameters(
        serde_json::from_value(serde_json::json!({
            "to": "carol",
            "message": "from bob",
        }))
        .expect("decode request"),
    ))
    .await
    .expect("bob sends");

    let filtered = carol
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({ "from": "alice" })).expect("decode"),
        ))
        .await
        .expect("filtered read succeeds");
    let body = call_text(&filtered);
    assert!(body.contains("1 message(s):"), "body: {body}");
    assert!(body.contains("From: alice"), "body: {body}");
    assert!(!body.contains("From: bob"), "body: {body}");

    // Bob's message is still pending — the filter does not drain it.
    let remaining = carol
        .read_messages(Parameters(
            serde_json::from_value(serde_json::json!({})).expect("decode"),
        ))
        .await
        .expect("unfiltered read succeeds");
    assert!(
        call_text(&remaining).contains("From: bob"),
        "body: {}",
        call_text(&remaining)
    );

    alice.daemon().close().await;
    bob.daemon().close().await;
    carol.daemon().close().await;
    handle.shutdown().await;
}
