//! Marker-delimited `ax` section inside the runtime's instruction file
//! (CLAUDE.md / AGENTS.md).
//!
//! The literal Korean-language contract strings are kept verbatim —
//! any drift here would silently retrain downstream agents.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ax_agent::{Runtime, SUPPORTED_RUNTIMES};

const AX_MARKER_START: &str = "<!-- ax:instructions:start -->";
const AX_MARKER_END: &str = "<!-- ax:instructions:end -->";

#[derive(Debug, thiserror::Error)]
pub enum InstructionsError {
    #[error("unknown runtime {0:?}")]
    UnknownRuntime(String),
    #[error("read {path:?}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("write {path:?}: {source}")]
    Write { path: PathBuf, source: io::Error },
}

/// Render the managed ax section for `workspace` into the runtime's
/// instruction file. Also strips stale ax sections out of every
/// *other* supported runtime's file in `dir` so switching the runtime
/// doesn't leave dead instructions behind.
pub fn write_instructions(
    dir: &Path,
    workspace: &str,
    runtime: &str,
    custom_instructions: &str,
) -> Result<(), InstructionsError> {
    let runtime_enum = Runtime::normalize(runtime)
        .ok_or_else(|| InstructionsError::UnknownRuntime(runtime.to_owned()))?;
    let target_file = runtime_enum.instruction_file();
    let target = dir.join(target_file);

    for other in SUPPORTED_RUNTIMES {
        if other == runtime_enum {
            continue;
        }
        strip_managed_section(&dir.join(other.instruction_file()))?;
    }

    let ax_section = format!(
        "{start}\n## ax workspace: {workspace}\n\n{body}\n{end}",
        start = AX_MARKER_START,
        body = managed_workspace_instructions(custom_instructions),
        end = AX_MARKER_END,
    );

    let content = match fs::read_to_string(&target) {
        Ok(existing) => splice_managed_section(&existing, &ax_section),
        Err(e) if e.kind() == io::ErrorKind::NotFound => format!("{ax_section}\n"),
        Err(source) => {
            return Err(InstructionsError::Read {
                path: target,
                source,
            });
        }
    };
    fs::write(&target, content).map_err(|source| InstructionsError::Write {
        path: target,
        source,
    })
}

/// Strip the managed ax section from every supported runtime file in
/// `dir`. Files that don't exist or that don't contain the markers are
/// left untouched.
pub fn remove_instructions(dir: &Path) -> Result<(), InstructionsError> {
    for runtime in SUPPORTED_RUNTIMES {
        strip_managed_section(&dir.join(runtime.instruction_file()))?;
    }
    Ok(())
}

// ---------- internals ----------

fn splice_managed_section(existing: &str, ax_section: &str) -> String {
    if let (Some(start), Some(end)) = (existing.find(AX_MARKER_START), existing.find(AX_MARKER_END))
    {
        let before = &existing[..start];
        let after = &existing[end + AX_MARKER_END.len()..];
        return format!("{before}{ax_section}{after}");
    }
    let trimmed = existing.trim_end_matches('\n');
    format!("{trimmed}\n\n{ax_section}\n")
}

fn strip_managed_section(path: &Path) -> Result<(), InstructionsError> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(InstructionsError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let Some(start) = content.find(AX_MARKER_START) else {
        return Ok(());
    };
    let Some(end) = content.find(AX_MARKER_END) else {
        return Ok(());
    };
    let before = content[..start].trim_end_matches('\n').to_owned();
    let after = content[end + AX_MARKER_END.len()..]
        .trim_start_matches('\n')
        .to_owned();
    if before.is_empty() && after.is_empty() {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(InstructionsError::Write {
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
    let mut out = before;
    if !after.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&after);
    }
    out.push('\n');
    fs::write(path, out).map_err(|source| InstructionsError::Write {
        path: path.to_owned(),
        source,
    })
}

fn managed_workspace_instructions(custom: &str) -> String {
    let mut sections: Vec<String> = Vec::with_capacity(5);
    let trimmed = custom.trim();
    if !trimmed.is_empty() {
        sections.push(trimmed.to_owned());
    }
    sections.push(durable_memory_instruction_contract());
    sections.push(message_handling_instruction_contract());
    sections.push(task_intake_instruction_contract());
    sections.push(completion_reporting_instruction_contract());
    sections.join("\n\n")
}

fn durable_memory_instruction_contract() -> String {
    [
        "## Durable Memory Contract",
        "- 런타임 native memory나 resume 품질에만 의존하지 말고, 재시작 이후에도 유지돼야 할 사실은 `remember_memory`로 ax daemon에 기록하세요.",
        "- 세션이 fresh/restart 되었거나 컨텍스트가 비어 보이면 먼저 `recall_memories(scopes=[\"global\",\"project\",\"workspace\"])`로 durable memory를 복원하세요.",
        "- 현재 durable memory 상태를 점검하거나 감사할 때는 `list_memories`를 사용하고, 현재 작업에 필요한 working set만 가져올 때는 `recall_memories`를 사용하세요.",
        "- 팀 전체 공통 규칙은 `scope=\"global\"`, 현재 프로젝트 결정/제약/핸드오프는 `scope=\"project\"`, 현재 워크스페이스 로컬 운영 메모는 `scope=\"workspace\"`를 우선 사용하세요.",
        "- 이미 저장한 기억이 더 이상 유효하지 않으면 `supersede_memory`를 사용해 교체하세요. 필요하면 저수준 경로로 `remember_memory(..., supersedes_ids=[...])`를 직접 써도 됩니다.",
        "- 매 응답 전에 무조건 메모리를 읽을 필요는 없습니다. fresh start, owner handoff, 설계 결정 확인, 반복되는 사용자 선호 복원처럼 실제로 가치가 있을 때만 recall 하세요.",
    ]
    .join("\n")
}

fn message_handling_instruction_contract() -> String {
    [
        "## Message Handling Contract",
        "- 수신 작업을 처리할 때는 `read_messages`로 최신 메시지를 확인하고, 새 작업 요청, 명시적 질문, 새 사실, 요청한 증거가 있을 때만 회신하세요.",
        "- 결과나 추가 정보가 필요할 때만 `send_message`로 회신하세요. 단순 ACK/수신 확인/감사/상태 핑만의 메시지는 보내지 마세요.",
        "- 진행 상태 공유가 필요하면 `send_message` 대신 `set_status`를 사용하세요.",
        "- 처리 결과는 현재 작업을 요청한 발신자에게만 보내고, 새 작업/새 사실/명시적 질문/요청한 증거가 없으면 침묵을 기본값으로 두세요.",
        "- `read_messages`에서 받은 최신 메시지가 이전에 처리한 메시지와 실질적으로 동일하거나, 지금 보내려는 응답이 이전 응답과 실질적으로 동일하면 회신하지 마세요.",
        "- `\"no new work\"`, `\"nothing to do\"`, `\"대기 중\"`, `\"진행 상황 없음\"`, `\"확인했습니다\"`, `\"thanks\"`, `\"ok\"` 같은 no-op 상태 메시지에는 회신하지 마세요.",
        "- `request` 툴의 반환값은 새 메시지가 아닙니다. 그 결과를 받았다고 다시 `send_message`를 보내지 마세요.",
    ]
    .join("\n")
}

fn task_intake_instruction_contract() -> String {
    [
        "## Task Intake Contract",
        "- 메시지에 `Task ID:`가 있으면, 전달되었거나 `read_messages`로 읽었다는 사실만으로 task를 claim한 것으로 간주하지 마세요.",
        "- 먼저 `get_task`로 task 문맥을 확인하세요.",
        "- 그 직후 첫 task-flow action은 정확히 다음 4가지 중 하나여야 합니다:",
        "  1. `update_task(..., status=\"in_progress\", log=\"mode=implementation|inspection; scope=<exact files/modules>; validation=<plan>\")`",
        "  2. exact blocker 또는 owner mismatch 보고",
        "  3. superseded/invalid/fail 명시 후 종료",
        "  4. structured evidence와 함께 completion",
        "- owner mismatch나 missing dependency가 보이면 fail fast 하세요. 다른 owner/API/file이 필요한지 구체적으로 적고 task를 오래 붙잡지 마세요.",
        "- 같은 `Task ID:`에 대해 substantive result를 이미 보냈다면, 그 뒤 도착한 concise current-status re-ask에는 같은 요약을 반복하지 말고 새 delta가 있을 때만 회신하세요.",
    ]
    .join("\n")
}

fn completion_reporting_instruction_contract() -> String {
    [
        "## Completion Reporting Contract",
        "- `update_task(..., status=\"completed\", result=...)` 또는 completion 회신 전에는 현재 scope 기준으로 남은 owned dirty/uncommitted files가 있는지 확인하세요.",
        "- completion result에는 반드시 다음 둘 중 하나를 포함하세요: `remaining owned dirty files=<none>` 또는 `remaining owned dirty files=<paths>; residual scope=<why work remains>`.",
        "- commit/task slice만 끝났다면 전체 요청이 끝난 것처럼 쓰지 말고, 이번에 끝난 unit과 남은 owned work를 구분해서 적으세요.",
        "- leftover owned work가 남아 있는데 설명 없이 `completed`나 \"done\"처럼 쓰지 마세요. 후속 unit, 범위 밖 항목, blocker 중 무엇인지 명시하세요.",
        "- `update_task(..., status=\"completed\", ..., confirm=true)`로 호출해야 daemon이 수락합니다. `confirm=true`는 다음 self-check를 마쳤다는 **명시적 affirmation**입니다 — 반사적으로 붙이지 말고 정말 확인 후에만 true로 두세요:",
        "  1. 약속한 파일 변경이 전부 저장/커밋되었음.",
        "  2. 관련 테스트/빌드가 통과했거나, 통과하지 않는 이유가 result에 적혀 있음.",
        "  3. result에 `remaining owned dirty files=` 마커가 올바른 모양으로 들어가 있음.",
        "  4. 이 task scope 안에서 남은 TODO/미해결 blocker가 없음 (있다면 failed나 후속 unit으로 넘어감).",
        "- `confirm` 없이(또는 `confirm=false`) completion을 보내면 daemon이 `CompletionRequiresConfirmation` 에러로 거부하고 체크리스트를 돌려줍니다. 그 메시지를 읽고 실제로 점검한 뒤에 `confirm=true`로 재호출하세요.",
        "- result에는 가능하면 **구체 evidence** 한 조각 이상을 남기세요. 예: 수정한 파일 경로 (`src/foo.rs`, `greeter/hello.sh`), 실행한 검증 커맨드 (`cargo test`, `pytest`, `npm test`), 또는 관련 git 동작. 파일 경로/커맨드 흔적이 없는 completion은 daemon이 거부하진 않지만 task 로그에 `evidence hint` 경고를 남겨서 리뷰어가 spot-check 하도록 표시합니다.",
    ]
    .join("\n")
}
