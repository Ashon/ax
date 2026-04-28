# Tasks / Durable Memory

`ax`는 메시지 기반 협업 위에 두 개의 durable layer를 추가합니다.

- task: 실행 단위 추적
- memory: 재시작 이후에도 남겨야 할 결정/사실/제약

## Task

task는 단순 메시지보다 강한 추적 단위입니다.

주요 특징:

- assignee / creator 추적
- status / result / log 기록
- parent-child rollup
- stale 신호 계산
- recovery action (`wake`, `interrupt`, `retry`)
- fresh start와 workflow mode 메타데이터

### 주요 MCP 도구

| 도구 | 용도 |
|---|---|
| `create_task` | pending task record만 생성. inbox message / wake / `Task ID:` 주입 없음 |
| `start_task` | task 생성 + `Task ID:` 주입 dispatch + wake |
| `update_task` | status/result/log 업데이트 |
| `get_task` | 단건 상세 조회 |
| `list_tasks` | 전체 task 조회 |
| `list_workspace_tasks` | workspace 기준 조회 |
| `cancel_task` | 활성 task 취소 |
| `remove_task` | terminal task archive |
| `intervene_task` | wake / interrupt / retry |

### 권장 흐름

즉시 시작할 작업:

1. `start_task`
2. assignee가 `get_task`
3. assignee가 `update_task(..., status="in_progress", ...)`
4. 완료 시 `update_task(..., status="completed", result=...)`

기록만 먼저 남길 작업:

1. `create_task`
2. 소비자 / orchestrator가 `list_tasks` 또는 `list_workspace_tasks`로 조회
3. 나중에 별도 dispatch 또는 follow-up

### task와 message queue 경계

`read_messages`는 workspace inbox만 읽습니다. `create_task`로 만든 pending task는
inbox에 들어가지 않기 때문에, `read_messages`가 비어 있어도 assigned pending task가
남아 있을 수 있습니다.

대기 작업을 복구하거나 wake prompt 이후 no-work 여부를 판단할 때는 다음 조회를 함께
확인합니다.

```text
list_workspace_tasks(workspace="<self>", view="assigned", status="pending")
list_tasks(assignee="<self>", status="pending")
```

반대로 `send_message`는 일반 메시지만 만들며 task state를 생성하거나 갱신하지 않습니다.
즉시 실행과 추적이 모두 필요한 작업은 `start_task`를 사용합니다.

### start mode

- `default`: 기존 session reuse
- `fresh`: session을 fresh-context로 다시 시작한 뒤 처리

### workflow mode

- `parallel`: 기본값
- `serial`: sibling child task의 순차 실행 의도를 나타내는 메타데이터. 현재 daemon이 sibling
  dispatch를 자동 gate/release하지는 않으므로 orchestrator가 진행 순서를 관리해야 합니다.

### 저장 위치

- durable state: `tasks-state.json`
- 관측용 snapshot: `tasks.json`

즉, watch/status 계열은 snapshot을 읽고, daemon은 durable state를 기준으로 동작합니다.

## Durable Memory

memory는 “runtime native memory에만 맡기면 안 되는 정보”를 daemon에 남기는 계층입니다.

적합한 예:

- 설계 결정
- 사용자 선호
- 프로젝트 제약
- handoff 메모
- 반복적으로 회수해야 하는 운영 사실

### 주요 MCP 도구

| 도구 | 용도 |
|---|---|
| `remember_memory` | 새 메모리 저장 |
| `recall_memories` | 현재 작업에 필요한 working set 조회 |
| `list_memories` | 메모리 상태 감사/점검 |
| `supersede_memory` | 이전 메모리를 새 메모리로 교체 |

### scope

MCP에서는 alias를 쓸 수 있습니다.

- `global`
- `project`
- `workspace`

또는 explicit selector:

- `project:alpha`
- `workspace:api.backend`
- `task:<id>`

### kind 예시

- `decision`
- `fact`
- `constraint`
- `handoff`
- `preference`

### 예시

새 프로젝트 결정 저장:

```text
remember_memory(
  scope="project",
  kind="decision",
  subject="Auth",
  content="Use the shared gateway for API authentication.",
  tags=["auth", "gateway"]
)
```

작업 전 관련 기억 복구:

```text
recall_memories(scopes=["global", "project", "workspace"])
```

현재 메모리 상태 점검:

```text
list_memories(scopes=["project"], include_superseded=true)
```

오래된 기억 교체:

```text
supersede_memory(
  scope="project",
  kind="decision",
  subject="Release",
  content="Use staged rollout automation instead of manual Friday releases.",
  supersedes_ids=["<old-memory-id>"]
)
```

### orchestrator와 worker의 차이

- orchestrator prompt에는 현재 관련 durable memory 요약이 주입됩니다.
- worker prompt에는 “언제 / 어떻게 recall 할지” 규칙만 들어갑니다.
- worker는 필요할 때 `recall_memories` 또는 `list_memories`를 호출해 복구합니다.

이렇게 한 이유:

- 메모리 변경만으로 workspace instruction hash가 흔들리는 것을 피함
- reconcile / restart churn 최소화

### 저장 위치

- durable store: `memories.json`

## 언제 task를 쓰고 언제 memory를 쓰는가

task는 “이번 실행 단위”, memory는 “다음 실행에도 남겨야 할 사실”입니다.

- 현재 구현 작업 상태 추적: task
- 다음 세션에서도 살아 있어야 할 결정/제약: memory
- 둘 다 필요한 경우: task 결과를 바탕으로 확정 사실만 memory로 승격
