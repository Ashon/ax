//! Marker-delimited instruction section behaviour. Ports the three Go
//! tests in `internal/workspace/instructions_test.go` plus a coverage
//! test for the cleanup pass over stale runtime files.

use std::fs;

use ax_workspace::{remove_instructions, write_instructions};

const MANAGED_HEADERS: &[&str] = &[
    "## Durable Memory Contract",
    "## Message Handling Contract",
    "## Task Intake Contract",
    "## Completion Reporting Contract",
];

#[test]
fn codex_runtime_emits_agents_md_with_every_contract() {
    let dir = tempfile::tempdir().unwrap();
    write_instructions(
        dir.path(),
        "ax.runtime",
        "codex",
        "Follow local ownership rules.",
    )
    .unwrap();
    let text = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
    for want in [
        "Follow local ownership rules.",
        "`remember_memory`",
        "`recall_memories(scopes=[\"global\",\"project\",\"workspace\"])`",
        "`list_memories`",
        "`supersede_memory`",
        "`scope=\"project\"`",
        "단순 ACK/수신 확인/감사/상태 핑만의 메시지는 보내지 마세요.",
        "`set_status`",
        "`request` 툴의 반환값은 새 메시지가 아닙니다.",
        "메시지에 `Task ID:`가 있으면, 전달되었거나 `read_messages`로 읽었다는 사실만으로 task를 claim한 것으로 간주하지 마세요.",
        "`get_task`로 task 문맥을 확인",
        "`update_task(..., status=\"in_progress\"",
        "`remaining owned dirty files=<none>`",
        "이번에 끝난 unit과 남은 owned work를 구분해서 적으세요.",
        "owner mismatch나 missing dependency가 보이면 fail fast",
        "concise current-status re-ask에는 같은 요약을 반복하지 말고 새 delta가 있을 때만 회신",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    for header in MANAGED_HEADERS {
        assert!(text.contains(header), "missing header {header} in:\n{text}");
    }
}

#[test]
fn blank_custom_body_still_writes_every_contract() {
    let dir = tempfile::tempdir().unwrap();
    write_instructions(dir.path(), "ax.runtime", "claude", "").unwrap();
    let text = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
    assert!(text.contains("## ax workspace: ax.runtime"));
    for header in MANAGED_HEADERS {
        assert!(text.contains(header));
    }
}

#[test]
fn second_write_replaces_without_duplicating_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("CLAUDE.md");
    fs::write(&path, "Existing intro\n").unwrap();

    write_instructions(dir.path(), "ax.runtime", "claude", "First body.").unwrap();
    write_instructions(dir.path(), "ax.runtime", "claude", "Second body.").unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("First body."));
    for header in MANAGED_HEADERS {
        assert_eq!(
            text.matches(header).count(),
            1,
            "expected one `{header}` section, got {} in:\n{text}",
            text.matches(header).count(),
        );
    }
    // Non-ax intro is preserved verbatim.
    assert!(text.contains("Existing intro"));
}

#[test]
fn runtime_switch_strips_stale_section_from_other_runtime_file() {
    let dir = tempfile::tempdir().unwrap();
    write_instructions(dir.path(), "worker", "claude", "claude body").unwrap();
    assert!(dir.path().join("CLAUDE.md").exists());

    write_instructions(dir.path(), "worker", "codex", "codex body").unwrap();
    // AGENTS.md now carries the managed section.
    assert!(fs::read_to_string(dir.path().join("AGENTS.md"))
        .unwrap()
        .contains("## Durable Memory Contract"));
    // CLAUDE.md was deleted because its entire content was the managed section.
    assert!(!dir.path().join("CLAUDE.md").exists());
}

#[test]
fn runtime_switch_preserves_non_managed_content_in_stale_file() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("CLAUDE.md");
    fs::write(&claude, "Existing intro\n").unwrap();
    write_instructions(dir.path(), "worker", "claude", "claude body").unwrap();

    write_instructions(dir.path(), "worker", "codex", "codex body").unwrap();
    // File still exists; only the managed section is gone.
    let remaining = fs::read_to_string(&claude).unwrap();
    assert!(remaining.contains("Existing intro"));
    assert!(!remaining.contains("Durable Memory Contract"));
}

#[test]
fn remove_instructions_strips_every_runtime_file() {
    let dir = tempfile::tempdir().unwrap();
    write_instructions(dir.path(), "worker", "claude", "").unwrap();
    remove_instructions(dir.path()).unwrap();
    // With no non-managed content left, the file is removed.
    assert!(!dir.path().join("CLAUDE.md").exists());
}

#[test]
fn unknown_runtime_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = write_instructions(dir.path(), "worker", "nonesuch", "").unwrap_err();
    assert!(
        err.to_string().contains("nonesuch"),
        "error should mention the runtime name: {err}"
    );
}
