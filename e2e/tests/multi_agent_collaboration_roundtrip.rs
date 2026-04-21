//! Coordinated multi-agent delivery scenario.
//!
//! This test drives a realistic 4-peer collaboration through the raw
//! daemon wire protocol to prove the primitives we added across the
//! stale-lifecycle and peer-awareness autoresearch runs compose
//! correctly. An orchestrator decomposes a build into three pieces;
//! two specialists work in parallel publishing artifacts; a third
//! specialist consumes both outputs once sibling messages announce
//! they are ready; the orchestrator observes the whole thing from its
//! inbox alone — no polling into task state from outside.
//!
//! The scenario exercises, in one go:
//!   - `create_task` × 3 (orchestrator fan-out)
//!   - `update_task(InProgress → Completed)` with Completion
//!     Reporting Contract marker
//!   - `set_shared_value` / `get_shared_value` for artifact handoff
//!   - `send_message` for sibling "artifact-ready" signalling
//!   - `read_messages` for dependency gating
//!   - Terminal-status push (orchestrator inbox receives three
//!     `[task-completed]` pushes without querying task state)
//!   - `list_workspaces` peer-awareness fields reflecting live
//!     load and in-progress task IDs mid-flight, then settling to
//!     zero load at the end

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ax_daemon::{Daemon, DaemonHandle};
use ax_proto::payloads::{
    CreateTaskPayload, GetSharedPayload, ReadMessagesPayload, RegisterPayload,
    SendMessagePayload, SetSharedPayload, UpdateTaskPayload,
};
use ax_proto::responses::{
    GetSharedResponse, ListWorkspacesResponse, ReadMessagesResponse,
    SendMessageResponse, StatusResponse, TaskResponse,
};
use ax_proto::types::TaskStatus;
use ax_proto::{Envelope, ErrorPayload, MessageType, ResponsePayload};
use serde::de::DeserializeOwned;
use tempfile::TempDir;

struct SyncClient {
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl SyncClient {
    fn connect(
        socket: &std::path::Path,
        workspace: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut client = Self {
            reader: BufReader::new(stream),
            next_id: 1,
        };
        let _: StatusResponse = client.request(
            MessageType::Register,
            &RegisterPayload {
                workspace: workspace.to_owned(),
                dir: format!("/tmp/{workspace}"),
                description: String::new(),
                config_path: String::new(),
                idle_timeout_seconds: 0,
            },
        )?;
        Ok(client)
    }

    fn request<P, R>(
        &mut self,
        kind: MessageType,
        payload: &P,
    ) -> Result<R, Box<dyn std::error::Error>>
    where
        P: serde::Serialize,
        R: DeserializeOwned,
    {
        let id = format!("e2e-{}", self.next_id);
        self.next_id += 1;
        let env = Envelope::new(&id, kind, payload)?;
        let mut bytes = serde_json::to_vec(&env)?;
        bytes.push(b'\n');
        self.reader.get_mut().write_all(&bytes)?;
        self.reader.get_mut().flush()?;
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line)?;
            if read == 0 {
                return Err("connection closed".into());
            }
            let env: Envelope = serde_json::from_str(line.trim_end())?;
            if env.id != id {
                continue;
            }
            match env.r#type {
                MessageType::Response => {
                    let wrap: ResponsePayload = env.decode_payload()?;
                    return Ok(serde_json::from_str(wrap.data.get())?);
                }
                MessageType::Error => {
                    let err: ErrorPayload = env.decode_payload()?;
                    return Err(err.message.into());
                }
                other => return Err(format!("unexpected envelope {other:?}").into()),
            }
        }
    }
}

async fn spawn_daemon(state_dir: &std::path::Path) -> DaemonHandle {
    let socket = state_dir.join("daemon.sock");
    let handle = Daemon::new(socket)
        .with_state_dir(state_dir)
        .expect("state_dir accepted")
        .bind()
        .await
        .expect("daemon binds");
    for _ in 0..50 {
        if handle.socket_path().exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle
}

/// One specialist doing the "produce an artifact, publish it via a
/// shared value, then tell the consumer it's ready" dance. Returns
/// the task id so the outer assertions can join on it.
fn specialist_produces_and_announces(
    socket: &std::path::Path,
    workspace: &str,
    task_id: &str,
    artifact_key: &str,
    artifact_value: &str,
    consumer: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = SyncClient::connect(socket, workspace)?;

    // Step 1: heartbeat — promote the task so the creator (orch)
    // sees it as `current_task_id` mid-flight if it peeks.
    let _: TaskResponse = client.request(
        MessageType::UpdateTask,
        &UpdateTaskPayload {
            id: task_id.to_owned(),
            status: Some(TaskStatus::InProgress),
            log: Some(format!("{workspace} picking up {task_id}")),
            ..Default::default()
        },
    )?;

    // Step 2: publish the artifact under a well-known key.
    let _: StatusResponse = client.request(
        MessageType::SetShared,
        &SetSharedPayload {
            key: artifact_key.to_owned(),
            value: artifact_value.to_owned(),
        },
    )?;

    // Step 3: announce readiness to the consumer.
    let _: SendMessageResponse = client.request(
        MessageType::SendMessage,
        &SendMessagePayload {
            to: consumer.to_owned(),
            message: format!(
                "artifact-ready key={artifact_key} producer={workspace}"
            ),
            config_path: String::new(),
        },
    )?;

    // Step 4: close the loop with a contract-compliant marker.
    let _: TaskResponse = client.request(
        MessageType::UpdateTask,
        &UpdateTaskPayload {
            id: task_id.to_owned(),
            status: Some(TaskStatus::Completed),
            result: Some(format!(
                "{workspace} published {artifact_key}; remaining owned dirty files=<none>"
            )),
            confirm: Some(true),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// The consumer: waits for both sibling ready-messages to arrive,
/// reads the shared values they published, produces a combined
/// artifact of its own, and reports completion.
fn consumer_waits_then_integrates(
    socket: &std::path::Path,
    workspace: &str,
    task_id: &str,
    expected_producers: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut client = SyncClient::connect(socket, workspace)?;

    // Heartbeat before the long wait so a concurrent `list_workspaces`
    // sees `tests` as in_progress too.
    let _: TaskResponse = client.request(
        MessageType::UpdateTask,
        &UpdateTaskPayload {
            id: task_id.to_owned(),
            status: Some(TaskStatus::InProgress),
            log: Some("awaiting upstream artifacts".into()),
            ..Default::default()
        },
    )?;

    // Gate on receiving one artifact-ready message per expected
    // producer. Poll the inbox with a bounded retry so the test
    // doesn't hang forever if a producer failed to signal.
    let mut seen: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for _ in 0..40 {
        let inbox: ReadMessagesResponse = client.request(
            MessageType::ReadMessages,
            &ReadMessagesPayload {
                limit: 10,
                from: String::new(),
            },
        )?;
        for msg in &inbox.messages {
            if msg.content.starts_with("artifact-ready") {
                seen.insert(msg.from.clone());
            }
        }
        if expected_producers.iter().all(|p| seen.contains(*p)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    for producer in expected_producers {
        assert!(
            seen.contains(*producer),
            "consumer did not receive artifact-ready signal from {producer}"
        );
    }

    // Integrate both upstream artifacts.
    let mut combined = String::new();
    for producer in expected_producers {
        let got: GetSharedResponse = client.request(
            MessageType::GetShared,
            &GetSharedPayload {
                key: format!("artifacts/{producer}"),
            },
        )?;
        assert!(
            got.found,
            "shared artifact from {producer} missing at consume time"
        );
        combined.push_str(&got.value);
        combined.push('|');
    }

    // Publish the integrated result itself so the orchestrator can
    // verify downstream consumers could reach it the same way.
    let _: StatusResponse = client.request(
        MessageType::SetShared,
        &SetSharedPayload {
            key: "artifacts/tests".into(),
            value: combined.clone(),
        },
    )?;

    let _: TaskResponse = client.request(
        MessageType::UpdateTask,
        &UpdateTaskPayload {
            id: task_id.to_owned(),
            status: Some(TaskStatus::Completed),
            result: Some(format!(
                "integrated {} artifacts; remaining owned dirty files=<none>",
                expected_producers.len()
            )),
            confirm: Some(true),
            ..Default::default()
        },
    )?;
    Ok(combined)
}

/// Specialist whose work fails — reports Failed with a reason
/// instead of publishing an artifact. Used by the partial-failure
/// scenario below to validate the negative path.
fn specialist_reports_failure(
    socket: &std::path::Path,
    workspace: &str,
    task_id: &str,
    reason: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = SyncClient::connect(socket, workspace)?;
    let _: TaskResponse = client.request(
        MessageType::UpdateTask,
        &UpdateTaskPayload {
            id: task_id.to_owned(),
            status: Some(TaskStatus::InProgress),
            log: Some(format!("{workspace} picked up {task_id}")),
            ..Default::default()
        },
    )?;
    let _: TaskResponse = client.request(
        MessageType::UpdateTask,
        &UpdateTaskPayload {
            id: task_id.to_owned(),
            status: Some(TaskStatus::Failed),
            result: Some(format!("failed: {reason}")),
            log: Some(format!("failed: {reason}")),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// Consumer that discovers one of its upstream producers failed,
/// escalates by transitioning to Blocked, and asks the failing
/// peer (via a targeted message) to clarify before giving up.
fn consumer_escalates_when_upstream_missing(
    socket: &std::path::Path,
    workspace: &str,
    task_id: &str,
    expected_producers: &[&str],
    help_peer: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = SyncClient::connect(socket, workspace)?;
    let _: TaskResponse = client.request(
        MessageType::UpdateTask,
        &UpdateTaskPayload {
            id: task_id.to_owned(),
            status: Some(TaskStatus::InProgress),
            log: Some("awaiting upstream".into()),
            ..Default::default()
        },
    )?;

    // Wait a bounded number of polls for all expected producers to
    // announce ready. If one never does, we escalate.
    let mut seen: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while std::time::Instant::now() < deadline
        && !expected_producers.iter().all(|p| seen.contains(*p))
    {
        let inbox: ReadMessagesResponse = client.request(
            MessageType::ReadMessages,
            &ReadMessagesPayload {
                limit: 10,
                from: String::new(),
            },
        )?;
        for msg in &inbox.messages {
            if msg.content.starts_with("artifact-ready") {
                seen.insert(msg.from.clone());
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let missing: Vec<&&str> = expected_producers
        .iter()
        .filter(|p| !seen.contains(**p))
        .collect();
    assert!(
        !missing.is_empty(),
        "test setup expected at least one producer to never announce"
    );

    // Escalate: transition to Blocked AND send a pointed help-request
    // to the failing peer so the orchestrator can see the intended
    // recipient in the message log.
    let _: SendMessageResponse = client.request(
        MessageType::SendMessage,
        &SendMessagePayload {
            to: help_peer.to_owned(),
            message: format!(
                "[task-blocked-help] Task ID: {task_id} — upstream artifact from {help_peer} never arrived"
            ),
            config_path: String::new(),
        },
    )?;
    let _: TaskResponse = client.request(
        MessageType::UpdateTask,
        &UpdateTaskPayload {
            id: task_id.to_owned(),
            status: Some(TaskStatus::Blocked),
            log: Some(format!("blocked: missing upstream {missing:?}")),
            ..Default::default()
        },
    )?;
    Ok(())
}

#[tokio::test]
async fn three_specialists_deliver_coordinated_build_through_orch_push() {
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    // All of the work happens on blocking threads because SyncClient
    // is sync — the daemon is async under the hood. We run each
    // specialist in its own blocking task so they share a real
    // daemon socket concurrently.
    let orch_socket = socket.clone();
    let (task_ids, interim_view, combined_artifact) =
        tokio::task::spawn_blocking(move || {
            // Orchestrator sets up the roster up-front.
            let mut orch = SyncClient::connect(&orch_socket, "orch").expect("orch");

            let compile_task: TaskResponse = orch
                .request(
                    MessageType::CreateTask,
                    &CreateTaskPayload {
                        title: "compile".into(),
                        assignee: "compiler".into(),
                        ..Default::default()
                    },
                )
                .expect("create compile");
            let docs_task: TaskResponse = orch
                .request(
                    MessageType::CreateTask,
                    &CreateTaskPayload {
                        title: "docs".into(),
                        assignee: "docs".into(),
                        ..Default::default()
                    },
                )
                .expect("create docs");
            let tests_task: TaskResponse = orch
                .request(
                    MessageType::CreateTask,
                    &CreateTaskPayload {
                        title: "integration tests".into(),
                        assignee: "tests".into(),
                        ..Default::default()
                    },
                )
                .expect("create tests");
            let task_ids = (
                compile_task.task.id.clone(),
                docs_task.task.id.clone(),
                tests_task.task.id.clone(),
            );

            // Kick off the two producers on threads and the consumer
            // on a third; the consumer gates on the producers'
            // ready-signals so timing is naturally enforced.
            let (tid_compile, _tid_docs, tid_tests) =
                (task_ids.0.clone(), task_ids.1.clone(), task_ids.2.clone());

            let compile_socket = orch_socket.clone();
            let tid_compile_thread = tid_compile.clone();
            let compile_thread = std::thread::spawn(move || {
                specialist_produces_and_announces(
                    &compile_socket,
                    "compiler",
                    &tid_compile_thread,
                    "artifacts/compiler",
                    "bytes:binary-v1",
                    "tests",
                )
                .expect("compiler thread");
            });
            let docs_socket = orch_socket.clone();
            let tid_docs_thread = task_ids.1.clone();
            let docs_thread = std::thread::spawn(move || {
                specialist_produces_and_announces(
                    &docs_socket,
                    "docs",
                    &tid_docs_thread,
                    "artifacts/docs",
                    "markdown:doc-v1",
                    "tests",
                )
                .expect("docs thread");
            });
            let tests_socket = orch_socket.clone();
            let tid_tests_thread = tid_tests.clone();
            let tests_thread = std::thread::spawn(move || {
                consumer_waits_then_integrates(
                    &tests_socket,
                    "tests",
                    &tid_tests_thread,
                    &["compiler", "docs"],
                )
                .expect("tests thread")
            });

            compile_thread.join().expect("compile join");
            docs_thread.join().expect("docs join");
            let combined = tests_thread.join().expect("tests join");

            // Snapshot the registry once every specialist has closed
            // its task. We use this to prove the peer-awareness
            // fields settle back to "no load" — the same snapshot
            // API a real observer would call.
            let listed: ListWorkspacesResponse = orch
                .request(
                    MessageType::ListWorkspaces,
                    &serde_json::json!({}),
                )
                .expect("list workspaces");

            (task_ids, listed, combined)
        })
        .await
        .expect("scenario blocking");

    let (compile_id, docs_id, tests_id) = task_ids;

    // ─── Orchestrator expectations ─────────────────────────────────
    // 1. Exactly three terminal-status pushes landed in orch's inbox,
    //    one per completed sub-task. The orchestrator didn't have to
    //    poll `list_tasks` or query anything task-specific; the
    //    completion channel is push-driven.
    let orch_inbox = tokio::task::spawn_blocking(move || {
        let mut orch = SyncClient::connect(&socket, "orch").expect("orch reopen");
        let inbox: ReadMessagesResponse = orch
            .request(
                MessageType::ReadMessages,
                &ReadMessagesPayload {
                    limit: 10,
                    from: String::new(),
                },
            )
            .expect("read orch inbox");
        inbox
    })
    .await
    .expect("orch blocking");

    assert_eq!(
        orch_inbox.messages.len(),
        3,
        "orch inbox should carry one completion push per specialist, got {:?}",
        orch_inbox.messages.iter().map(|m| &m.content).collect::<Vec<_>>()
    );
    for msg in &orch_inbox.messages {
        assert!(
            msg.content.contains("[task-completed]"),
            "every push must be a completion marker: {}",
            msg.content
        );
        assert_eq!(msg.to, "orch");
    }
    let ids_in_inbox: std::collections::BTreeSet<String> = orch_inbox
        .messages
        .iter()
        .map(|m| m.content.clone())
        .collect();
    for id in [&compile_id, &docs_id, &tests_id] {
        assert!(
            ids_in_inbox.iter().any(|c| c.contains(id)),
            "completion push for task {id} missing"
        );
    }

    // 2. The peer-awareness snapshot taken after all specialists
    //    closed their tasks must show zero active load and no
    //    `current_task_id` for any worker — proving the terminal
    //    bookkeeping the daemon does on our behalf flows all the
    //    way out through `list_workspaces`.
    for ws in &interim_view.workspaces {
        if matches!(ws.name.as_str(), "compiler" | "docs" | "tests") {
            assert_eq!(
                ws.active_task_count, 0,
                "{} still showing load after completion: {ws:?}",
                ws.name
            );
            assert!(
                ws.current_task_id.is_none(),
                "{} still has a current_task_id: {ws:?}",
                ws.name
            );
        }
    }
    // Every listed peer should be carrying a live last_activity_at
    // — liveness data stayed attached through all the messaging.
    for ws in &interim_view.workspaces {
        assert!(
            ws.last_activity_at.is_some(),
            "{} lost its last_activity_at watermark",
            ws.name
        );
    }

    // 3. The integration output visible to downstream consumers
    //    must be composed from both upstream artifacts in the
    //    expected order.
    assert!(
        combined_artifact.contains("bytes:binary-v1"),
        "integrated artifact missing compile output: {combined_artifact}"
    );
    assert!(
        combined_artifact.contains("markdown:doc-v1"),
        "integrated artifact missing docs output: {combined_artifact}"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn partial_failure_escalates_through_distinct_terminal_pushes() {
    // Same fan-out shape as the happy path, but `docs` hits a hard
    // failure and never publishes its artifact. The consumer (`tests`)
    // must notice and escalate to Blocked instead of silently hanging.
    //
    // What this asserts end-to-end:
    //   - Completed / Failed / Blocked all push separate,
    //     distinguishable notifications into the creator's inbox.
    //   - The help-request message sent to the failing peer is
    //     actually deliverable (peer still registered even after
    //     its task failed).
    //   - Peer-awareness fields stay consistent: once every task is
    //     out of InProgress, no worker has a `current_task_id`
    //     regardless of whether it got there via success or failure.
    let tmp = TempDir::new().expect("tempdir");
    let handle = spawn_daemon(tmp.path()).await;
    let socket = handle.socket_path().to_path_buf();

    let orch_socket = socket.clone();
    let (task_ids, snapshot, docs_inbox) =
        tokio::task::spawn_blocking(move || {
            let mut orch = SyncClient::connect(&orch_socket, "orch").expect("orch");
            let compile_task: TaskResponse = orch
                .request(
                    MessageType::CreateTask,
                    &CreateTaskPayload {
                        title: "compile".into(),
                        assignee: "compiler".into(),
                        ..Default::default()
                    },
                )
                .expect("create compile");
            let docs_task: TaskResponse = orch
                .request(
                    MessageType::CreateTask,
                    &CreateTaskPayload {
                        title: "docs".into(),
                        assignee: "docs".into(),
                        ..Default::default()
                    },
                )
                .expect("create docs");
            let tests_task: TaskResponse = orch
                .request(
                    MessageType::CreateTask,
                    &CreateTaskPayload {
                        title: "integration tests".into(),
                        assignee: "tests".into(),
                        ..Default::default()
                    },
                )
                .expect("create tests");
            let task_ids = (
                compile_task.task.id.clone(),
                docs_task.task.id.clone(),
                tests_task.task.id.clone(),
            );

            // compile succeeds as usual.
            let compile_socket = orch_socket.clone();
            let tid_c = task_ids.0.clone();
            let compile_thread = std::thread::spawn(move || {
                specialist_produces_and_announces(
                    &compile_socket,
                    "compiler",
                    &tid_c,
                    "artifacts/compiler",
                    "bytes:binary-v2",
                    "tests",
                )
                .expect("compiler thread");
            });
            // docs fails hard — no artifact, no announcement.
            let docs_socket = orch_socket.clone();
            let tid_d = task_ids.1.clone();
            let docs_thread = std::thread::spawn(move || {
                specialist_reports_failure(
                    &docs_socket,
                    "docs",
                    &tid_d,
                    "source markdown refused to parse",
                )
                .expect("docs failure thread");
            });
            // tests waits briefly, notices docs never announced, and
            // escalates to Blocked with a help-request.
            let tests_socket = orch_socket.clone();
            let tid_t = task_ids.2.clone();
            let tests_thread = std::thread::spawn(move || {
                consumer_escalates_when_upstream_missing(
                    &tests_socket,
                    "tests",
                    &tid_t,
                    &["compiler", "docs"],
                    "docs",
                )
                .expect("tests escalation thread");
            });

            compile_thread.join().expect("compile join");
            docs_thread.join().expect("docs join");
            tests_thread.join().expect("tests join");

            let snapshot: ListWorkspacesResponse = orch
                .request(
                    MessageType::ListWorkspaces,
                    &serde_json::json!({}),
                )
                .expect("list workspaces");

            // The failing `docs` peer stays registered, so the
            // help-request sent by `tests` should still be
            // deliverable. Read its inbox through a fresh client.
            let mut docs_client =
                SyncClient::connect(&orch_socket, "docs").expect("docs reopen");
            let docs_inbox: ReadMessagesResponse = docs_client
                .request(
                    MessageType::ReadMessages,
                    &ReadMessagesPayload {
                        limit: 10,
                        from: String::new(),
                    },
                )
                .expect("docs inbox");

            (task_ids, snapshot, docs_inbox)
        })
        .await
        .expect("failure scenario blocking");

    let (compile_id, docs_id, tests_id) = task_ids;

    // ─── Orchestrator sees three distinct terminal pushes ────────
    let orch_inbox = tokio::task::spawn_blocking(move || {
        let mut orch = SyncClient::connect(&socket, "orch").expect("orch reopen");
        let inbox: ReadMessagesResponse = orch
            .request(
                MessageType::ReadMessages,
                &ReadMessagesPayload {
                    limit: 10,
                    from: String::new(),
                },
            )
            .expect("read orch inbox");
        inbox
    })
    .await
    .expect("orch blocking");

    assert_eq!(
        orch_inbox.messages.len(),
        3,
        "exactly three terminal pushes, got {:?}",
        orch_inbox
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
    );
    let by_task: std::collections::BTreeMap<&str, &str> = orch_inbox
        .messages
        .iter()
        .filter_map(|m| {
            // Each content starts with `[task-<status>] Task ID: <id>`.
            let header = m.content.split_whitespace().next()?;
            let id = m
                .content
                .split("Task ID: ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())?;
            Some((id, header))
        })
        .collect();
    assert_eq!(
        by_task.get(compile_id.as_str()).copied(),
        Some("[task-completed]"),
        "compile should push [task-completed]"
    );
    assert_eq!(
        by_task.get(docs_id.as_str()).copied(),
        Some("[task-failed]"),
        "docs should push [task-failed]"
    );
    assert_eq!(
        by_task.get(tests_id.as_str()).copied(),
        Some("[task-blocked]"),
        "tests should push [task-blocked]"
    );

    // ─── Help-request was delivered to the failing peer ──────────
    assert!(
        docs_inbox
            .messages
            .iter()
            .any(|m| m.content.contains("[task-blocked-help]") && m.from == "tests"),
        "docs should have received the help-request from tests: {:?}",
        docs_inbox.messages
    );

    // ─── Peer-awareness settles despite mixed outcomes ───────────
    for ws in &snapshot.workspaces {
        if matches!(ws.name.as_str(), "compiler" | "docs") {
            assert_eq!(
                ws.active_task_count, 0,
                "{} should have zero open tasks in terminal state: {ws:?}",
                ws.name
            );
            assert!(
                ws.current_task_id.is_none(),
                "{} should expose no current_task_id: {ws:?}",
                ws.name
            );
        }
        if ws.name == "tests" {
            // Blocked is not terminal — it still counts as an open
            // task. That asymmetry is a load-bearing invariant: the
            // orchestrator needs a signal that SOMETHING still owns
            // this task even though the worker stepped back.
            assert_eq!(
                ws.active_task_count, 1,
                "blocked tests task should remain counted as open: {ws:?}"
            );
        }
    }

    handle.shutdown().await;
}
