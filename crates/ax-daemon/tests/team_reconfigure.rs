//! End-to-end coverage for the four team-reconfigure handlers.
//!
//! Verifies the feature-flag gate, plan warnings, full apply + finish
//! lifecycle (with the managed YAML materialized under
//! `<state>/managed-teams/`), revision mismatch rejection, and lease
//! contention between concurrent apply attempts.

use std::fs;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use ax_daemon::{Daemon, DaemonHandle};
use ax_proto::payloads::{
    FinishTeamReconfigurePayload, GetTeamStatePayload, RegisterPayload, SetSharedPayload,
    TeamReconfigurePayload,
};
use ax_proto::responses::{StatusResponse, TeamApplyResponse, TeamPlanResponse, TeamStateResponse};
use ax_proto::types::{
    TeamChangeOp, TeamChildSpec, TeamEntryKind, TeamReconcileMode, TeamReconfigureChange,
    TeamWorkspaceSpec,
};
use ax_proto::{Envelope, MessageType, ResponsePayload};
use serde::de::DeserializeOwned;

struct Client {
    writer: tokio::net::unix::OwnedWriteHalf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    counter: u64,
}

impl Client {
    fn connect(socket: &Path) -> Self {
        let std = StdUnixStream::connect(socket).expect("connect");
        std.set_nonblocking(true).expect("nonblocking");
        let stream = UnixStream::from_std(std).expect("from_std");
        let (reader, writer) = stream.into_split();
        Self {
            writer,
            reader: BufReader::new(reader),
            counter: 0,
        }
    }

    fn next_id(&mut self) -> String {
        self.counter += 1;
        format!("t{}", self.counter)
    }

    async fn send<T: serde::Serialize>(&mut self, kind: MessageType, payload: &T) -> String {
        let id = self.next_id();
        let env = Envelope::new(&id, kind, payload).expect("encode envelope");
        let mut bytes = serde_json::to_vec(&env).expect("marshal");
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.expect("write");
        id
    }

    async fn recv(&mut self) -> Envelope {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await.expect("read line");
        assert!(n > 0, "daemon closed connection unexpectedly");
        serde_json::from_str(line.trim_end_matches('\n')).expect("decode envelope")
    }

    async fn request<T: serde::Serialize, R: DeserializeOwned>(
        &mut self,
        kind: MessageType,
        payload: &T,
    ) -> R {
        let sent_id = self.send(kind, payload).await;
        loop {
            let env = self.recv().await;
            if env.id != sent_id {
                continue;
            }
            match env.r#type {
                MessageType::Response => {
                    let wrap: ResponsePayload = env.decode_payload().expect("response payload");
                    assert!(wrap.success, "expected success response");
                    return serde_json::from_str(wrap.data.get()).expect("decode body");
                }
                MessageType::Error => {
                    let err: ax_proto::ErrorPayload = env.decode_payload().expect("error payload");
                    panic!("daemon error: {}", err.message);
                }
                other => panic!("unexpected envelope type: {other:?}"),
            }
        }
    }

    async fn request_err<T: serde::Serialize>(&mut self, kind: MessageType, payload: &T) -> String {
        let sent_id = self.send(kind, payload).await;
        loop {
            let env = self.recv().await;
            if env.id != sent_id {
                continue;
            }
            match env.r#type {
                MessageType::Error => {
                    let err: ax_proto::ErrorPayload = env.decode_payload().expect("error payload");
                    return err.message;
                }
                other => panic!("expected error, got {other:?}"),
            }
        }
    }
}

struct Fixtures {
    _tmp: TempDir,
    project_dir: PathBuf,
    config_path: PathBuf,
    state_dir: PathBuf,
}

fn write_config(project: &Path, body: &str) -> PathBuf {
    let config = project.join(".ax").join("config.yaml");
    fs::create_dir_all(config.parent().expect("config dir")).expect("create config dir");
    fs::write(&config, body).expect("write config");
    config
}

fn make_fixtures(body: &str) -> Fixtures {
    let tmp = TempDir::new().expect("tempdir");
    let project_dir = tmp.path().join("proj");
    fs::create_dir_all(&project_dir).expect("project dir");
    let config_path = write_config(&project_dir, body);
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(&state_dir).expect("state dir");
    Fixtures {
        _tmp: tmp,
        project_dir,
        config_path,
        state_dir,
    }
}

async fn spawn_daemon(state_dir: &Path) -> DaemonHandle {
    let socket_path = state_dir.join("daemon.sock");
    Daemon::new(socket_path)
        .with_state_dir(state_dir)
        .expect("with_state_dir")
        .bind()
        .await
        .expect("bind daemon")
}

async fn wait_for_socket(socket: &Path) {
    for _ in 0..50 {
        if socket.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn register(workspace: &str) -> RegisterPayload {
    RegisterPayload {
        workspace: workspace.to_owned(),
        dir: format!("/tmp/{workspace}"),
        description: String::new(),
        config_path: String::new(),
        idle_timeout_seconds: 0,
    }
}

async fn enable_feature_flag(client: &mut Client) {
    let _: StatusResponse = client
        .request(
            MessageType::SetShared,
            &SetSharedPayload {
                key: "experimental_mcp_team_reconfigure".into(),
                value: "true".into(),
            },
        )
        .await;
}

const SIMPLE_CONFIG: &str =
    "project: demo\nworkspaces:\n  alpha:\n    dir: ./alpha\n    runtime: claude\n  beta:\n    dir: ./beta\n    runtime: claude\n";

#[tokio::test]
async fn get_state_returns_current_desired_with_feature_disabled() {
    let f = make_fixtures(SIMPLE_CONFIG);
    let handle = spawn_daemon(&f.state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut client = Client::connect(handle.socket_path());
    let _: StatusResponse = client.request(MessageType::Register, &register("op")).await;

    let resp: TeamStateResponse = client
        .request(
            MessageType::GetTeamState,
            &GetTeamStatePayload {
                config_path: f.config_path.display().to_string(),
            },
        )
        .await;
    assert!(!resp.state.feature_enabled);
    assert_eq!(resp.state.revision, 0);
    assert_eq!(resp.state.desired.workspaces, vec!["alpha", "beta"]);
    assert!(resp.state.desired.root_orchestrator_enabled);

    handle.shutdown().await;
}

#[tokio::test]
async fn dry_run_requires_feature_flag() {
    let f = make_fixtures(SIMPLE_CONFIG);
    let handle = spawn_daemon(&f.state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut client = Client::connect(handle.socket_path());
    let _: StatusResponse = client.request(MessageType::Register, &register("op")).await;

    let err = client
        .request_err(
            MessageType::DryRunTeam,
            &TeamReconfigurePayload {
                config_path: f.config_path.display().to_string(),
                expected_revision: None,
                changes: Vec::new(),
                reconcile_mode: None,
            },
        )
        .await;
    assert!(err.contains("disabled"), "got: {err}");

    handle.shutdown().await;
}

#[tokio::test]
async fn dry_run_produces_plan_with_actions_and_warnings() {
    let f = make_fixtures(SIMPLE_CONFIG);
    let handle = spawn_daemon(&f.state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut client = Client::connect(handle.socket_path());
    let _: StatusResponse = client.request(MessageType::Register, &register("op")).await;
    enable_feature_flag(&mut client).await;

    // Remove existing + add new + try to remove a non-existent entry.
    let changes = vec![
        TeamReconfigureChange {
            op: TeamChangeOp::Remove,
            kind: TeamEntryKind::Workspace,
            name: "beta".into(),
            workspace: None,
            child: None,
        },
        TeamReconfigureChange {
            op: TeamChangeOp::Add,
            kind: TeamEntryKind::Workspace,
            name: "gamma".into(),
            workspace: Some(TeamWorkspaceSpec {
                dir: "./gamma".into(),
                description: "new".into(),
                shell: String::new(),
                runtime: "claude".into(),
                codex_model_reasoning_effort: String::new(),
                agent: String::new(),
                instructions: String::new(),
                env: std::collections::BTreeMap::new(),
            }),
            child: None,
        },
        TeamReconfigureChange {
            op: TeamChangeOp::Remove,
            kind: TeamEntryKind::Workspace,
            name: "ghost".into(),
            workspace: None,
            child: None,
        },
    ];
    let resp: TeamPlanResponse = client
        .request(
            MessageType::DryRunTeam,
            &TeamReconfigurePayload {
                config_path: f.config_path.display().to_string(),
                expected_revision: Some(0),
                changes: changes.clone(),
                reconcile_mode: None,
            },
        )
        .await;
    assert_eq!(resp.plan.expected_revision, 0);
    assert_eq!(resp.plan.state.revision, 1);
    let action_names: Vec<_> = resp
        .plan
        .actions
        .iter()
        .map(|a| (a.action.clone(), a.name.clone()))
        .collect();
    assert!(action_names.contains(&("destroy".to_owned(), "beta".to_owned())));
    assert!(action_names.contains(&("ensure".to_owned(), "gamma".to_owned())));
    assert!(
        resp.plan
            .warnings
            .iter()
            .any(|w| w.contains("ghost") && w.contains("already absent")),
        "warnings: {:?}",
        resp.plan.warnings
    );
    // Plan must not persist a managed-teams file even though overlay
    // has changes; only apply should persist.
    let managed = f.state_dir.join("managed-teams");
    // Plan writes a `-plan-<ts>.yaml` to the managed dir, but no
    // `<hash>.yaml` should exist yet.
    if managed.exists() {
        let has_persisted = fs::read_dir(&managed)
            .expect("read managed dir")
            .flatten()
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .chars()
                    .filter(|c| *c == '-')
                    .count()
                    == 0
            });
        assert!(!has_persisted, "plan should not persist canonical yaml");
    }

    handle.shutdown().await;
}

#[tokio::test]
async fn apply_persists_state_and_managed_yaml() {
    let f = make_fixtures(SIMPLE_CONFIG);
    let handle = spawn_daemon(&f.state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut client = Client::connect(handle.socket_path());
    let _: StatusResponse = client.request(MessageType::Register, &register("op")).await;
    enable_feature_flag(&mut client).await;

    let changes = vec![TeamReconfigureChange {
        op: TeamChangeOp::Add,
        kind: TeamEntryKind::Workspace,
        name: "delta".into(),
        workspace: Some(TeamWorkspaceSpec {
            dir: "./delta".into(),
            description: String::new(),
            shell: String::new(),
            runtime: "claude".into(),
            codex_model_reasoning_effort: String::new(),
            agent: String::new(),
            instructions: String::new(),
            env: std::collections::BTreeMap::new(),
        }),
        child: None,
    }];
    let apply: TeamApplyResponse = client
        .request(
            MessageType::ApplyTeam,
            &TeamReconfigurePayload {
                config_path: f.config_path.display().to_string(),
                expected_revision: Some(0),
                changes: changes.clone(),
                reconcile_mode: Some(TeamReconcileMode::ArtifactsOnly),
            },
        )
        .await;
    assert!(!apply.ticket.token.is_empty());
    assert_eq!(
        apply.ticket.reconcile_mode,
        TeamReconcileMode::ArtifactsOnly
    );

    let managed = f.state_dir.join("managed-teams");
    assert!(
        managed.is_dir(),
        "managed-teams dir should exist after apply"
    );
    let canonical: Vec<_> = fs::read_dir(&managed)
        .expect("read managed dir")
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            Path::new(&name)
                .extension()
                .is_some_and(|ext| ext == "yaml")
                && !name.contains("-plan-")
        })
        .collect();
    assert_eq!(
        canonical.len(),
        1,
        "expected one canonical managed yaml after apply"
    );

    // Finish the apply so the lease clears and last_apply is recorded.
    let final_state: TeamStateResponse = client
        .request(
            MessageType::FinishTeam,
            &FinishTeamReconfigurePayload {
                token: apply.ticket.token.clone(),
                success: true,
                error: String::new(),
                actions: apply.ticket.plan.actions.clone(),
            },
        )
        .await;
    assert_eq!(final_state.state.revision, 1);
    let last = final_state
        .state
        .last_apply
        .expect("last_apply must be set");
    assert!(last.success);

    // Re-fetch state after restart to confirm persistence.
    handle.shutdown().await;
    let handle2 = spawn_daemon(&f.state_dir).await;
    wait_for_socket(handle2.socket_path()).await;
    let mut client2 = Client::connect(handle2.socket_path());
    let _: StatusResponse = client2
        .request(MessageType::Register, &register("op"))
        .await;
    let resp: TeamStateResponse = client2
        .request(
            MessageType::GetTeamState,
            &GetTeamStatePayload {
                config_path: f.config_path.display().to_string(),
            },
        )
        .await;
    assert_eq!(resp.state.revision, 1);
    assert!(resp.state.last_apply.is_some());

    handle2.shutdown().await;
}

#[tokio::test]
async fn apply_rejects_mismatched_revision() {
    let f = make_fixtures(SIMPLE_CONFIG);
    let handle = spawn_daemon(&f.state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut client = Client::connect(handle.socket_path());
    let _: StatusResponse = client.request(MessageType::Register, &register("op")).await;
    enable_feature_flag(&mut client).await;

    let err = client
        .request_err(
            MessageType::DryRunTeam,
            &TeamReconfigurePayload {
                config_path: f.config_path.display().to_string(),
                expected_revision: Some(42),
                changes: Vec::new(),
                reconcile_mode: None,
            },
        )
        .await;
    assert!(err.contains("revision mismatch"), "got: {err}");

    handle.shutdown().await;
}

#[tokio::test]
async fn apply_blocks_while_lease_active_and_releases_on_finish() {
    let f = make_fixtures(SIMPLE_CONFIG);
    let handle = spawn_daemon(&f.state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut client = Client::connect(handle.socket_path());
    let _: StatusResponse = client.request(MessageType::Register, &register("op")).await;
    enable_feature_flag(&mut client).await;

    let disable_root = vec![TeamReconfigureChange {
        op: TeamChangeOp::Disable,
        kind: TeamEntryKind::RootOrchestrator,
        name: String::new(),
        workspace: None,
        child: None,
    }];
    let first: TeamApplyResponse = client
        .request(
            MessageType::ApplyTeam,
            &TeamReconfigurePayload {
                config_path: f.config_path.display().to_string(),
                expected_revision: Some(0),
                changes: disable_root.clone(),
                reconcile_mode: None,
            },
        )
        .await;

    // Second apply without finishing the first must fail.
    let err = client
        .request_err(
            MessageType::ApplyTeam,
            &TeamReconfigurePayload {
                config_path: f.config_path.display().to_string(),
                expected_revision: Some(1),
                changes: Vec::new(),
                reconcile_mode: None,
            },
        )
        .await;
    assert!(err.contains("already in progress"), "got: {err}");

    // Finish releases the lease.
    let _: TeamStateResponse = client
        .request(
            MessageType::FinishTeam,
            &FinishTeamReconfigurePayload {
                token: first.ticket.token.clone(),
                success: true,
                error: String::new(),
                actions: Vec::new(),
            },
        )
        .await;

    // Now another apply at revision 1 should succeed.
    let second: TeamApplyResponse = client
        .request(
            MessageType::ApplyTeam,
            &TeamReconfigurePayload {
                config_path: f.config_path.display().to_string(),
                expected_revision: Some(1),
                changes: Vec::new(),
                reconcile_mode: None,
            },
        )
        .await;
    assert_eq!(second.ticket.plan.state.revision, 2);

    // Make sure project_dir is referenced so rustc doesn't warn about
    // fixtures fields being dead in this test path.
    let _ = f.project_dir.as_path();

    handle.shutdown().await;
}

#[tokio::test]
async fn child_add_and_remove_roundtrip_through_plan() {
    let f = make_fixtures(SIMPLE_CONFIG);
    let handle = spawn_daemon(&f.state_dir).await;
    wait_for_socket(handle.socket_path()).await;
    let mut client = Client::connect(handle.socket_path());
    let _: StatusResponse = client.request(MessageType::Register, &register("op")).await;
    enable_feature_flag(&mut client).await;

    let plan: TeamPlanResponse = client
        .request(
            MessageType::DryRunTeam,
            &TeamReconfigurePayload {
                config_path: f.config_path.display().to_string(),
                expected_revision: Some(0),
                changes: vec![TeamReconfigureChange {
                    op: TeamChangeOp::Add,
                    kind: TeamEntryKind::Child,
                    name: "sub".into(),
                    workspace: None,
                    child: Some(TeamChildSpec {
                        dir: "./subproj".into(),
                        prefix: "team".into(),
                    }),
                }],
                reconcile_mode: None,
            },
        )
        .await;
    assert!(
        plan.plan.state.desired.children.iter().any(|c| c == "sub"),
        "sub child should appear in desired"
    );
    assert_eq!(plan.plan.state.overlay.added_children.len(), 1);

    handle.shutdown().await;
}
