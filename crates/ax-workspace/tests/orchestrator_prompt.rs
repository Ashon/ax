use std::fs;
use std::path::Path;

use ax_config::{ProjectNode, WorkspaceRef};
use ax_workspace::{orchestrator_prompt, write_orchestrator_prompt};

fn memory_json(scope: &str, subject: &str, content: &str) -> String {
    format!(
        concat!(
            "{{",
            "\"id\":\"mem-1\",",
            "\"scope\":\"{scope}\",",
            "\"kind\":\"decision\",",
            "\"subject\":\"{subject}\",",
            "\"content\":\"{content}\",",
            "\"tags\":[\"auth\"],",
            "\"created_by\":\"alpha.orchestrator\",",
            "\"created_at\":\"2026-04-18T00:00:00Z\",",
            "\"updated_at\":\"2026-04-18T00:00:00Z\"",
            "}}"
        ),
        scope = scope,
        subject = subject,
        content = content,
    )
}

#[test]
fn write_orchestrator_prompt_distinguishes_alias_and_project_name() {
    let child = ProjectNode {
        name: "shared".to_owned(),
        alias: "alpha".to_owned(),
        prefix: "alpha".to_owned(),
        workspaces: vec![WorkspaceRef {
            name: "worker".to_owned(),
            merged_name: "alpha.worker".to_owned(),
            ..WorkspaceRef::default()
        }],
        ..ProjectNode::default()
    };

    let sub_dir = tempfile::tempdir().unwrap();
    fs::write(sub_dir.path().join("AGENTS.md"), "stale").unwrap();
    write_orchestrator_prompt(
        sub_dir.path(),
        &child,
        &child.prefix,
        "orchestrator",
        "claude",
        Path::new("/tmp/ax.sock"),
    )
    .unwrap();
    let sub_text = fs::read_to_string(sub_dir.path().join("CLAUDE.md")).unwrap();
    for want in [
        "# ax sub orchestrator: alpha (shared)",
        "당신은 `alpha (shared)` 프로젝트의 서브 오케스트레이터입니다.",
        "부모 트리에서의 별칭: `alpha`",
        "실제 프로젝트 이름: `shared`",
    ] {
        assert!(
            sub_text.contains(want),
            "expected sub prompt to contain {want:?}\n{sub_text}"
        );
    }
    assert!(!sub_dir.path().join("AGENTS.md").exists());

    let root = ProjectNode {
        name: "root".to_owned(),
        children: vec![child],
        ..ProjectNode::default()
    };

    let root_dir = tempfile::tempdir().unwrap();
    write_orchestrator_prompt(
        root_dir.path(),
        &root,
        "",
        "",
        "claude",
        Path::new("/tmp/ax.sock"),
    )
    .unwrap();
    let root_text = fs::read_to_string(root_dir.path().join("CLAUDE.md")).unwrap();
    assert!(
        root_text.contains("| **alpha (shared)** | `alpha.orchestrator` | worker |"),
        "expected root prompt to list child display identity, got:\n{root_text}"
    );
}

#[test]
fn orchestrator_prompt_requires_tracking_assigned_work_to_closure() {
    let root = ProjectNode {
        name: "root".to_owned(),
        ..ProjectNode::default()
    };
    let root_prompt = orchestrator_prompt(&root, "", "");
    for want in [
        "`read_messages`가 비어 있어도 \"작업 없음\"으로 결론내리기 전",
        "`list_tasks(assignee=\"orchestrator\", status=\"pending\")`",
        "`list_tasks(assignee=\"orchestrator\", status=\"in_progress\")`",
        "runnable task는 `get_task`로 구조화된 문맥을 확인",
        "오케스트레이터는 자신이 assign한 일이 실제 완료 결과, 명시적 blocker 보고, 실패 중 하나의 종결 상태에 도달할 때까지 계속 추적할 책임이 있습니다.",
        "assign한 일은 실제 완료 증거를 받거나, blocker를 상위에 명시적으로 보고하거나, 실패로 종료할 때까지 계속 소유하고 추적합니다.",
        "`remaining owned dirty files=<none|paths>`",
        "남은 owned dirty files가 있으면 residual scope 또는 후속 unit이 명시될 때만 부분 완료로 다루세요.",
    ] {
        assert!(
            root_prompt.contains(want),
            "expected root prompt to contain {want:?}\n{root_prompt}"
        );
    }

    let child = ProjectNode {
        name: "shared".to_owned(),
        prefix: "shared".to_owned(),
        ..ProjectNode::default()
    };
    let sub_prompt = orchestrator_prompt(&child, &child.prefix, "orchestrator");
    for want in [
        "`read_messages`가 비어 있어도 \"작업 없음\"으로 결론내리기 전",
        "`list_tasks(assignee=\"shared.orchestrator\", status=\"pending\")`",
        "`list_tasks(assignee=\"shared.orchestrator\", status=\"in_progress\")`",
        "runnable task는 `get_task`로 구조화된 문맥을 확인",
        "오케스트레이터는 자신이 assign한 일이 실제 완료 결과, 명시적 blocker 보고, 실패 중 하나의 종결 상태에 도달할 때까지 계속 추적할 책임이 있습니다.",
        "assign한 일은 실제 완료 증거를 받거나, blocker를 상위에 명시적으로 보고하거나, 실패로 종료할 때까지 계속 소유하고 추적합니다.",
        "`remaining owned dirty files=<none|paths>`",
        "남은 owned dirty files가 있으면 residual scope 또는 후속 unit이 명시될 때만 부분 완료로 다루세요.",
    ] {
        assert!(
            sub_prompt.contains(want),
            "expected sub prompt to contain {want:?}\n{sub_prompt}"
        );
    }
}

#[test]
fn write_orchestrator_prompt_includes_durable_memory_section() {
    let state_dir = tempfile::tempdir().unwrap();
    let socket_path = state_dir.path().join("daemon.sock");
    let memories = format!(
        "[{}]",
        memory_json(
            "project:alpha",
            "Auth",
            "Use the shared gateway for API authentication."
        )
    );
    fs::write(state_dir.path().join("memories.json"), memories).unwrap();

    let child = ProjectNode {
        name: "shared".to_owned(),
        alias: "alpha".to_owned(),
        prefix: "alpha".to_owned(),
        ..ProjectNode::default()
    };
    let sub_dir = tempfile::tempdir().unwrap();
    write_orchestrator_prompt(
        sub_dir.path(),
        &child,
        &child.prefix,
        "orchestrator",
        "claude",
        &socket_path,
    )
    .unwrap();
    let text = fs::read_to_string(sub_dir.path().join("CLAUDE.md")).unwrap();
    for want in [
        "## Durable Memory",
        "`global`, `project:alpha`, `workspace:alpha.orchestrator`",
        "Use the shared gateway for API authentication.",
        "[decision] `project:alpha` Auth:",
        "`list_memories`",
        "`supersede_memory`",
    ] {
        assert!(
            text.contains(want),
            "expected durable memory prompt section to contain {want:?}\n{text}"
        );
    }
}
