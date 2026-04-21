//! Cross-transport coverage for the `plan_initial_team` and
//! `plan_team_reconfigure` MCP tools. Exercises the full
//! rmcp duplex path (serve → `call_tool` → dispatch → JSON response)
//! so regressions in the tool wiring, not just the planner
//! internals, surface here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use rmcp::{ClientHandler, ServiceExt};
use tempfile::TempDir;

use ax_daemon::{Daemon, DaemonHandle};
use ax_mcp_server::{DaemonClient, InitialTeamPlan, ReconfigureTeamPlan, Server};

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

fn seed_role_project(root: &Path) {
    std::fs::create_dir_all(root.join("frontend")).unwrap();
    std::fs::create_dir_all(root.join("backend")).unwrap();
    std::fs::create_dir_all(root.join("infra")).unwrap();
    std::fs::write(root.join("frontend/package.json"), "{}").unwrap();
    std::fs::write(root.join("backend/go.mod"), "module demo\n").unwrap();
    std::fs::write(root.join("infra/main.tf"), "terraform {}\n").unwrap();
    std::fs::write(
        root.join("README.md"),
        "# demo\n\nWeb stack split by layer.\n",
    )
    .unwrap();
}

fn seed_reconfigure_project(root: &Path) -> PathBuf {
    seed_role_project(root);
    let cfg_dir = root.join(".ax");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let cfg_path = cfg_dir.join("config.yaml");
    std::fs::write(
        &cfg_path,
        "# axis: role\n# rationale: layer split.\nproject: demo\n\
workspaces:\n  frontend:\n    dir: ./frontend\n    description: UI layer\n    runtime: codex\n  \
stale_removed:\n    dir: ./gone\n    description: removed long ago\n    runtime: codex\n",
    )
    .unwrap();
    cfg_path
}

async fn connect_pair(
    handle: &DaemonHandle,
    cfg_path: Option<&Path>,
) -> (
    rmcp::service::RunningService<rmcp::RoleClient, StubClient>,
    tokio::task::JoinHandle<()>,
) {
    let daemon = DaemonClient::builder(handle.socket_path(), "alpha")
        .dir("/tmp/alpha")
        .connect()
        .await
        .expect("daemon client connects");
    let mut server = Server::new(daemon);
    if let Some(p) = cfg_path {
        server = server.with_config_path(p);
    }
    let (server_transport, client_transport) = tokio::io::duplex(16 * 1024);
    let server_handle = tokio::spawn(async move {
        let running = server.serve(server_transport).await.unwrap();
        let _ = running.waiting().await;
    });
    let client = StubClient
        .serve(client_transport)
        .await
        .expect("client handshake");
    (client, server_handle)
}

fn extract_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.raw.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn plan_initial_team_returns_axis_and_toplevel_dirs() {
    let tmp = TempDir::new().expect("tempdir");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    seed_role_project(&project);

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let handle = spawn_daemon(&state_dir).await;

    let (client, server_task) = connect_pair(&handle, None).await;
    let req = CallToolRequestParams::new("plan_initial_team").with_arguments(
        serde_json::json!({
            "project_dir": project.display().to_string(),
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let resp = client.call_tool(req).await.expect("call succeeds");
    let text = extract_text(&resp);
    let plan: InitialTeamPlan = serde_json::from_str(&text).expect("parse plan");

    assert_eq!(plan.suggested_axis, "role");
    let names: Vec<_> = plan.toplevel_dirs.iter().map(|d| d.name.clone()).collect();
    assert!(names.contains(&"frontend".to_owned()));
    assert!(names.contains(&"backend".to_owned()));
    assert!(names.contains(&"infra".to_owned()));

    drop(client);
    let _ = server_task.await;
    handle.shutdown().await;
}

#[tokio::test]
async fn plan_team_reconfigure_surfaces_orphans_and_empty_workspaces() {
    let tmp = TempDir::new().expect("tempdir");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let cfg_path = seed_reconfigure_project(&project);

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let handle = spawn_daemon(&state_dir).await;

    let (client, server_task) = connect_pair(&handle, Some(&cfg_path)).await;
    let req = CallToolRequestParams::new("plan_team_reconfigure").with_arguments(
        serde_json::json!({
            "project_dir": project.display().to_string(),
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let resp = client.call_tool(req).await.expect("call succeeds");
    let text = extract_text(&resp);
    let plan: ReconfigureTeamPlan = serde_json::from_str(&text).expect("parse plan");

    assert_eq!(plan.current_axis.as_deref(), Some("role"));
    assert!(plan.orphan_dirs.contains(&"backend".to_owned()));
    assert!(plan.orphan_dirs.contains(&"infra".to_owned()));
    assert!(plan.empty_workspaces.contains(&"stale_removed".to_owned()));

    drop(client);
    let _ = server_task.await;
    handle.shutdown().await;
}
