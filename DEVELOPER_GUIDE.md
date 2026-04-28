# ax Developer Guide

> 참고: 사용자/운영 문서는 `docs/` 아래로 정리되었습니다. 이 파일은 구현과 코드 구조 중심의 심화 레퍼런스로 유지합니다.

tmux 기반 멀티 에이전트 LLM 워크스페이스 매니저

## 목차

- [프로젝트 개요](#프로젝트-개요)
- [아키텍처](#아키텍처)
- [프로젝트 구조](#프로젝트-구조)
- [핵심 개념](#핵심-개념)
- [개발 환경 설정](#개발-환경-설정)
- [빌드 및 테스트](#빌드-및-테스트)
- [설정 파일](#설정-파일)
- [CLI 명령어](#cli-명령어)
- [데몬 프로토콜](#데몬-프로토콜)
- [MCP 도구](#mcp-도구)
- [에이전트 런타임](#에이전트-런타임)
- [계층적 프로젝트 구조](#계층적-프로젝트-구조)
- [릴리스](#릴리스)
- [코드 흐름 상세](#코드-흐름-상세)

---

## 프로젝트 개요

**ax**는 여러 LLM 에이전트(Claude, Codex)를 격리된 tmux 세션에서 동시에 실행하고, MCP(Model Context Protocol)를 통해 에이전트 간 통신을 가능하게 하는 워크스페이스 매니저이다.

주요 특징:

- tmux 세션 기반 에이전트 격리
- Unix 소켓 데몬을 통한 비동기 메시지 패싱
- task 생성/진행/완료 상태 추적과 stale 신호 계산
- MCP 표준 도구 인터페이스로 에이전트 간 통신
- 계층적 프로젝트 트리와 서브 오케스트레이터 지원
- ratatui 기반 모니터링 TUI (`ax watch` / `ax top`)

---

## 아키텍처

```
┌─────────────────────────────────────────────────────────────┐
│                         사용자                               │
│                 ax claude / ax codex / ax top                │
└──────────────┬──────────────────────────────┬───────────────┘
               │                              │
               ▼                              ▼
┌──────────────────────┐     ┌────────────────────────────────┐
│  orchestrator CLI    │     │         ax daemon              │
│  (foreground term)   │     │    (Unix socket server)        │
│                      │     │                                │
│  ┌────────────────┐  │     │  ┌──────────┐ ┌────────────┐  │
│  │  claude/codex  │  │     │  │ Registry │ │ MessageQueue│  │
│  │  CLI agent     │  │     │  └──────────┘ └────────────┘  │
│  └───────┬────────┘  │     │  ┌──────────┐ ┌────────────┐  │
│          │           │     │  │ History  │ │ TaskStore  │  │
│  ┌───────┴────────┐  │     │  └──────────┘ └────────────┘  │
│  │  MCP server    │──╋────▶│  ┌────────────┐ ┌────────────┐ │
│  │  (ax mcp-server)│ │     │  │SharedValues│ │WakeScheduler│ │
│  └────────────────┘  │     │  └────────────┘ └────────────┘ │
└──────────────────────┘     └────────────────────────────────┘
                                            ▲
                                            │
┌──────────────┐  ┌──────────────┐  ┌───────┴──────┐
│ workspace-A  │  │ workspace-B  │  │ workspace-C  │
│ (tmux)       │  │ (tmux)       │  │ (tmux)       │
│ claude + MCP │  │ codex + MCP  │  │ claude + MCP │
└──────────────┘  └──────────────┘  └──────────────┘
```

핵심 구성 요소:

| 구성 요소 | 역할 |
|-----------|------|
| **ax daemon** | Unix 소켓 기반 중앙 메시지 브로커. 워크스페이스 등록/해제, 메시지 큐, 공유 값 저장소, task 저장소, wake 재시도 스케줄링 |
| **MCP server** | 각 워크스페이스에 stdio로 연결되는 MCP 서버. 에이전트가 사용할 도구 노출 |
| **Workspace** | 하나의 tmux 세션 + 에이전트 런타임(claude/codex). 격리된 작업 환경 |
| **Orchestrator** | 다른 워크스페이스들을 조율하는 특수 에이전트. 자동 생성된 프롬프트로 역할 부여 |

---

## 프로젝트 구조

```
ax/
├── Cargo.toml                        # 워크스페이스 매니페스트
├── Cargo.lock
├── rust-toolchain.toml               # rustc 1.88
├── rustfmt.toml
├── Makefile                          # cargo 래퍼 (build / install / test)
├── .github/workflows/                # CI + release (모두 Rust)
│
├── crates/                           # Cargo workspace members
│   ├── ax-cli/                       #   바이너리 엔트리 (name = "ax")
│   │   └── src/main.rs               #     clap-free argv 파서 + 서브커맨드
│   ├── ax-tui/                       #   ratatui 기반 watch/top TUI
│   │   └── src/                      #     app/render/stream/tasks/tokens/theme/actions/captures
│   ├── ax-daemon/                    #   Unix 소켓 데몬
│   │   └── src/                      #     registry, queue, history, taskstore,
│   │                                 #     wake_scheduler, team_controller, handlers
│   ├── ax-mcp-server/                #   MCP stdio 서버 (33 tools)
│   │   └── src/                      #     server (rmcp), daemon_client, memory_scope
│   ├── ax-workspace/                 #   워크스페이스 생명주기 + 아티팩트
│   │   └── src/                      #     manager, reconcile, dispatch, lifecycle,
│   │                                 #     orchestrator{,_prompt}, mcp_config, instructions
│   ├── ax-config/                    #   .ax/config.yaml 스키마 + 트리 + overlay
│   │   └── src/                      #     schema, tree, paths, validate, overlay
│   ├── ax-agent/                     #   런타임 정규화 + launch 헬퍼
│   │   └── src/                      #     runtime(Claude/Codex), launch, claude, codex
│   ├── ax-tmux/                      #   tmux 세션 래퍼
│   │   └── src/                      #     commands, sessions, key tokens
│   ├── ax-proto/                     #   wire types (envelope, payloads, responses)
│   │   └── src/                      #     envelope, payloads, responses, types, usage
│   └── ax-usage/                     #   token 파서 + trend 집계
│       └── src/
│
└── e2e/                              # 크로스 크레이트 smoke + live 테스트
    ├── Cargo.toml
    └── tests/
        └── daemon_roundtrip.rs       #   register → send → read 엔드투엔드
```

---

## 핵심 개념

### Workspace

격리된 작업 환경 하나를 나타낸다. 각 워크스페이스는:

- 고유한 **tmux 세션**(`ax-<이름>`)에서 실행
- 자체 **에이전트 런타임**(claude 또는 codex)을 구동
- `.mcp.json`을 통해 **MCP 서버**에 연결
- 데몬에 **등록**되어 다른 워크스페이스와 메시지를 주고받음

### Orchestrator

다른 워크스페이스들을 조율하는 특수 에이전트이다. 역할별 프롬프트는 프로젝트 트리를 기반으로 자동 작성된다 (한국어).

- **루트 오케스트레이터**: 사용자 요청을 받아 워크스페이스/서브 오케스트레이터에 분배. **tmux 세션으로 자동 실행되지 않으며**, 사용자가 `ax claude` 또는 `ax codex`를 실행하면 해당 CLI가 `~/.ax/orchestrator/`에 있는 프롬프트와 MCP 설정을 상속받아 포그라운드 세션으로 루트 오케스트레이터 역할을 수행한다. CLI를 종료하면 루트 세션도 함께 종료된다.
- **서브 오케스트레이터**: 자체 프로젝트 범위의 워크스페이스를 관리하고 상위에 보고. `ax up` 시 프로젝트 트리를 순회하며 `<prefix>.orchestrator` 이름의 tmux 세션으로 상시 기동된다.

### Daemon

모든 워크스페이스 간 통신을 중개하는 서버이다.

- Unix 도메인 소켓(`~/.local/state/ax/daemon.sock`)으로 통신
- 뉴라인 구분 JSON 프로토콜
- 워크스페이스 레지스트리, 메시지 큐, 공유 값 저장소 관리
- `tasks.json` 기반 task 저장과 stale/queue/wake 관측 정보 계산
- unread 메시지에 대한 tmux wake 재시도 스케줄링
- PID 파일(`daemon.pid`)로 생존 확인

### MCP Server

각 워크스페이스에 stdio 방식으로 연결되는 MCP 서버이다.

- `ax mcp-server` 명령으로 실행되며, `.mcp.json`에 등록
- 데몬과 연결하여 에이전트에게 통신 도구를 노출
- 에이전트(claude/codex)는 이 도구들을 호출하여 다른 에이전트와 협업

---

## 개발 환경 설정

### 필수 요구 사항

| 도구 | 버전 | 용도 |
|------|------|------|
| **tmux** | 3.x+ | 세션 관리 |
| **claude** (CLI) | 최신 | Claude 런타임 에이전트 |

### 선택 사항

| 도구 | 용도 |
|------|------|
| **codex** | Codex 런타임 에이전트 |
| **goreleaser** | 멀티 플랫폼 바이너리 빌드 |

### 소스에서 빌드

```bash
git clone https://github.com/Ashon/ax.git
cd ax
make build
# ./ax 바이너리 생성
```

### 첫 프로젝트 생성

```bash
cd /path/to/your/project
./ax init       # 인터랙티브 설정 (.ax/config.yaml 생성)
./ax up         # 데몬 + 워크스페이스 + 서브 오케스트레이터 기동
./ax claude     # (또는 ./ax codex) 루트 오케스트레이터 CLI 실행
./ax top        # 워크스페이스 모니터링
./ax down       # 종료
```

---

## 빌드 및 테스트

### Makefile 타겟

```bash
make build      # cargo build --release --bin ax
make install    # ~/.cargo/bin/ax 에 설치 + ad-hoc codesign
make test       # cargo test
make clean      # 바이너리 + cargo clean
make release {patch|minor|major|dev}   # git tag 기반 릴리스
```

### 직접 빌드

```bash
cargo build --release --bin ax
cargo build --workspace
```

릴리스 프로파일은 `Cargo.toml`에서 thin LTO + single codegen unit으로 고정되어 있습니다.

### 테스트

```bash
cargo test --workspace                          # 전체 테스트
cargo test -p ax-config                         # config 크레이트만
cargo test -p ax-daemon                         # daemon 크레이트만
cargo test -p ax-daemon task_store::tests::refresh   # 특정 테스트
```

테스트 커버리지(대표):

- `crates/ax-config/tests/*`, `crates/ax-config/src/tree.rs` 내 `#[cfg(test)]`: 재귀적 자식 로딩, 순환 참조 방지
- `crates/ax-daemon/src/queue.rs`, `history.rs`, `shared_values.rs` 내 `#[cfg(test)]`: 프로토콜 직렬화, 레지스트리, 메시지 큐, 공유 값
- `crates/ax-daemon/src/task_store.rs`: task 기본값, 권한, 상태 전이, 중복 no-op 로그 억제
- `crates/ax-daemon/src/wake_scheduler.rs`, `task_helpers.rs`: stale/divergence/wake 상태 계산
- `crates/ax-proto/tests/`: wire 포맷 golden JSON fixture roundtrip

### 커스텀 provider live e2e

커스텀 LLM provider는 일반 CI와 live e2e를 분리해서 관리한다. 기본 테스트에는 URL, 모델명, API key 같은 환경별 값이 들어가면 안 된다. 기본 테스트는 YAML 파싱, provider merge, managed `CODEX_HOME/config.toml` 생성, `wire_api`, `web_search` 반영처럼 deterministic한 동작만 검증한다.

실제 endpoint 호출은 opt-in live e2e로만 실행한다. 테스트 코드는 `#[ignore]` 또는 `AX_LLM_E2E=1` 같은 명시적 플래그 없이는 skip되어야 하며, endpoint/model/key는 모두 환경 변수로 주입한다.

```bash
AX_LLM_E2E=1 \
AX_LLM_BASE_URL="http://127.0.0.1:8000/v1" \
AX_LLM_MODEL="local-model" \
AX_LLM_API_KEY_ENV="LOCAL_LLM_API_KEY" \
cargo test -p ax-e2e --test local_llm_provider -- --ignored --nocapture
```

live provider e2e 작성 규칙:

- repo에 내부 도메인, 실제 모델명, 실제 API key 이름을 커밋하지 않는다.
- 로그에는 전체 `base_url`이나 secret 값을 출력하지 않는다. 필요하면 host/path를 마스킹한다.
- 테스트는 임시 `CODEX_HOME`과 임시 project config를 만들고, host `~/.codex` 설정을 수정하지 않는다.
- OpenAI-compatible local endpoint는 Codex 현재 설정에 맞춰 `wire_api: responses`를 우선 사용한다.
- endpoint가 Codex 기본 tool을 거절할 수 있으므로 local provider 예시는 `web_search: disabled`를 기본으로 둔다.
- private/self-hosted runner나 protected environment에서만 live e2e를 실행한다. PR 기본 CI에는 포함하지 않는다.

---

## 설정 파일

### 위치

설정 파일은 다음 순서로 탐색된다 (현재 디렉터리부터 위로 올라가며 **가장 상위 조상**을 사용):

1. `.ax/config.yaml` (권장)
2. `ax.yaml` (레거시)

### 구조

```yaml
# .ax/config.yaml
project: my-project                  # 프로젝트 이름

orchestrator_runtime: claude         # 오케스트레이터 런타임 (선택)
default_agent_provider: local        # 기본 모델 provider (선택)

agent_providers:
  local:
    runtime: codex
    model: local-model
    base_url: http://127.0.0.1:8000/v1
    wire_api: responses
    web_search: disabled

workspaces:
  frontend:
    dir: ./frontend                  # 작업 디렉터리 (상대/절대/~ 경로)
    description: "React 프론트엔드"   # 에이전트 설명
    runtime: claude                  # claude 또는 codex (기본: claude)
    agent_provider: local            # agent_providers 키 (선택)
    instructions: |                  # 에이전트에게 전달할 지시사항
      React와 TypeScript를 사용합니다.
      테스트는 vitest로 실행합니다.
    env:                             # 환경 변수 (선택)
      NODE_ENV: development

  backend:
    dir: ./backend
    description: "Go API 서버"
    runtime: claude

  manual-shell:
    dir: .
    agent: none                      # 에이전트 자동 실행 안 함 (수동 셸)
    shell: /bin/zsh

  custom-agent:
    dir: .
    agent: "my-custom-agent --flag"  # 커스텀 에이전트 명령

children:                            # 자식 프로젝트 (계층 구조)
  sub-project:
    dir: ./services/sub-project      # 자식 config 위치
    prefix: sub                      # 워크스페이스 이름 접두사 (기본: 키 이름)
```

### Config 구조체 (`crates/ax-config/src/schema.rs`)

```rust
pub struct Config {
    pub project: String,
    pub orchestrator_runtime: String,
    pub default_agent_provider: String,
    pub disable_root_orchestrator: bool,
    pub experimental_mcp_team_reconfigure: bool,
    pub codex_model_reasoning_effort: String,
    pub idle_timeout_minutes: i32,
    pub max_orchestrator_depth: u32,
    pub max_children_per_node: u32,
    pub max_concurrent_agents: u32,
    pub agent_providers: BTreeMap<String, AgentProvider>,
    pub children: BTreeMap<String, Child>,
    pub workspaces: BTreeMap<String, Workspace>,
}

pub struct AgentProvider {
    pub runtime: String,
    pub model: String,
    pub base_url: String,
    pub env_key: String,
    pub wire_api: String,
    pub web_search: String,
}

pub struct Workspace {
    pub dir: String,
    pub description: String,
    pub shell: String,
    pub runtime: String,
    pub agent_provider: String,
    pub codex_model_reasoning_effort: String,
    pub agent: String,
    pub instructions: String,
    pub env: BTreeMap<String, String>,
}

pub struct Child {
    pub dir: String,
    pub prefix: String,
}
```

### 설정 로딩 과정

1. `find_config_file()`: 현재 디렉터리부터 위로 올라가며 가장 상위 조상의 config를 찾음
2. `Config::load(path)`: 재귀적으로 자식 config를 로드하여 워크스페이스를 병합
   - 자식 워크스페이스 이름은 `{prefix}.{name}` 형태로 병합
   - 순환 참조 감지 시 에러 반환
   - 누락된 자식은 경고 후 스킵
3. `load_tree(path)`: 병합 대신 계층 구조를 보존한 `ProjectNode` 트리 반환

---

## CLI 명령어

### 전역 플래그

```
--socket   데몬 소켓 경로 (기본: ~/.local/state/ax/daemon.sock)
--config   설정 파일 경로 (기본: 상위 디렉터리 자동 탐색)
```

### 명령어 목록

| 명령어 | 설명 |
|--------|------|
| `ax init` | 인터랙티브 프로젝트 초기화. Claude가 프로젝트를 분석해 설정 생성 |
| `ax up` | 데몬 시작 → 워크스페이스 생성 → 서브 오케스트레이터 기동 + 루트 오케스트레이터 산출물 갱신 |
| `ax down` | 모든 워크스페이스 / 서브 오케스트레이터 종료 → 데몬 정지 |
| `ax status` | 데몬/워크스페이스 상태, 프로젝트 트리 표시 |
| `ax claude [claude args...]` | 루트 오케스트레이터 프롬프트+MCP 설정을 그대로 가지고 Claude CLI를 포그라운드로 실행. 추가 인자는 그대로 전달 |
| `ax codex [codex args...]` | 루트 오케스트레이터 프롬프트+MCP 설정을 그대로 가지고 Codex CLI를 포그라운드로 실행. 추가 인자는 그대로 전달 |
| `ax top` | 워크스페이스 실시간 모니터링 TUI (`watch` alias 지원) |
| `ax send <workspace> <message>` | 메시지 전송 + 에이전트 웨이크 |
| `ax workspace create <name>` | 워크스페이스 수동 생성 |
| `ax workspace destroy <name>` | 워크스페이스 삭제 |
| `ax workspace list` | 활성 워크스페이스 목록 |
| `ax workspace attach <name>` | tmux 세션에 연결 |
| `ax workspace interrupt <name>` | 에이전트에 Escape 전송 |
| `ax daemon start` | 데몬 시작 |
| `ax daemon stop` | 데몬 정지 |
| `ax daemon status` | 데몬 상태 확인 |
| `ax run-agent` | (내부) 워크스페이스 내에서 에이전트 실행 |
| `ax mcp-server` | (내부) MCP 서버 시작 |

### TUI 메모

- `ax top`은 config 트리, 현재 tmux 세션, daemon workspace/task/message/usage 상태를 결합해 master/detail body를 그린다. 기본 tab은 `agents` / `messages` / `tasks` / `tokens`이며, agents quick action으로 workspace를 pin하면 `stream` tab이 추가된다.
- `ax top`의 task pane은 daemon state dir의 `tasks-state.json`을 우선 읽고 legacy `tasks.json`을 fallback으로 사용한다. active/stale/done/all 필터와 stale/divergence/queued message 배지를 표시한다.
- `ax shell`은 기본적으로 오케스트레이터 세션을 메인 pane에 보여주고, `Ctrl+A`로 control mode에 들어가 `v`(선택 워크스페이스 보기), `o`(오케스트레이터 복귀), `t`(stream 전환), `x`(interrupt) 같은 조작을 한다.
- `ax shell`도 `top`과 동일한 messages/tasks/tokens 스트림과 workspace status/task 관측 정보를 재사용한다.
- TUI 색상은 보조 신호다. 텍스트 label, `●`/`○` marker, bold/dim modifier, 선택 row의 reverse-video가 primary cue로 남아야 한다. `crates/ax-tui/src/theme.rs`가 foreground 색을 중앙 관리하며, renderer는 직접 `.fg(...)`를 흩뿌리지 말고 `theme::task_status`, `theme::agent_status`, `theme::sender`, `theme::git_state` 같은 semantic helper를 호출한다.
- semantic palette는 표준 terminal color만 사용한다. cyan은 focus/progress, green은 online/completed/clean/success, yellow는 blocked/stale/dirty/disconnected/high-priority/mixed git warning, red는 failed/error/panic, gray는 timestamp/separator/placeholder/metadata, dim gray는 disabled/offline, light blue/magenta/yellow/cyan은 workspace/sender/up token, task id/down token, cost, info 값에 쓴다.
- `NO_COLOR` 또는 `AX_TUI_NO_COLOR`가 설정되면 foreground 색을 비활성화한다. fallback에서도 bold, dim, reverse-video는 유지되므로 disabled/offline 값은 dim cue와 텍스트 label을 함께 가져야 하고, 선택 row는 색 없이도 보인다.
- agents view는 `NAME`, `STATE`, `UP`, `DOWN`, `COST`, `INFO` span 컬럼으로 렌더링한다. `NAME`은 cursor/indent/live marker/workspace label, `STATE`는 running/idle/online/offline/disconnected, `UP`/`DOWN`/`COST`는 live capture 또는 usage trend, `INFO`는 status_text와 reconcile/git note를 담당한다.
- agents git status는 leaf row마다 반복하지 않고 group/project row의 `INFO`에 roll-up한다. 직접 child workspace의 git 상태가 같으면 `git clean`, `git changed:N ?M` 또는 compact 폭의 `git ~N ?M`을 표시하고, 서로 다르면 warning style의 `git mixed`를 표시한다. 선택 workspace의 detail pane은 `modified`/`added`/`deleted`/`untracked`와 diff 통계를 포함한 full detail을 계속 보여준다.
- tasks view는 `ID`, `STATE`, `OWNER`, `TITLE` span 컬럼을 쓰며 state는 `pending`, `running`, `blocked`, `done`, `failed`, `cancelled`와 stale suffix를 텍스트로 보존한다. summary line은 `run`, `pend`, `stale`, `block`, `fail`, `done`, `msg`, `div`, `hi`, `cancel` 값을 각 semantic style로 구분한다.
- messages view는 timestamp, sender, recipient, optional short task id, body를 span으로 나누고, body는 error/failure/panic, blocked/warning/stale/wake, completed/success/done 계열 단어만 가볍게 분류한다. 이 분류는 routing/task linkage 색과 별개이며, 색 없이도 원문 body가 그대로 남아야 한다.
- 현재 자동 검증은 `cargo test -p ax-tui --lib`, `NO_COLOR=1 cargo test -p ax-tui --lib`, `cargo build --release --bin ax`, `git diff --check -- crates/ax-tui` 수준을 기준으로 한다. 실제 terminal theme별 시인성, focused/unfocused buffer snapshot, no-color screenshot 검증은 아직 수동 리뷰 영역이다.

---

## 데몬 프로토콜

데몬과 MCP 서버 간 통신은 Unix 소켓 위의 **뉴라인 구분 JSON** 형식을 사용한다.

### 엔벨로프 형식

```json
{
  "id": "uuid",
  "type": "message_type",
  "payload": { ... }
}
```

### 메시지 타입

| 타입 | 방향 | 설명 |
|------|------|------|
| `register` | Client → Daemon | 워크스페이스 등록 |
| `unregister` | Client → Daemon | 워크스페이스 해제 |
| `send_message` | Client → Daemon | 특정 워크스페이스에 메시지 전송 |
| `broadcast` | Client → Daemon | 모든 워크스페이스에 브로드캐스트 |
| `read_messages` | Client → Daemon | 대기 중인 inbox 메시지 읽기. task store 조회가 아님 |
| `list_workspaces` | Client → Daemon | 활성 워크스페이스 목록 |
| `set_status` | Client → Daemon | 워크스페이스 상태 텍스트 갱신 |
| `set_shared` | Client → Daemon | 공유 키-값 저장 |
| `get_shared` | Client → Daemon | 공유 키-값 조회 |
| `list_shared` | Client → Daemon | 모든 공유 값 목록 |
| `create_task` | Client → Daemon | 새 task 생성 |
| `update_task` | Client → Daemon | task 상태/결과/로그 갱신 |
| `get_task` | Client → Daemon | 단일 task 조회 |
| `list_tasks` | Client → Daemon | task 목록 조회 |
| `cancel_task` | Client → Daemon | active task 취소 |
| `remove_task` | Client → Daemon | terminal task 보관/숨김 |
| `intervene_task` | Client → Daemon | stuck task 복구 액션 적용 |
| `push_message` | Daemon → Client | 새 메시지 푸시 알림 |
| `response` | Daemon → Client | 요청 성공 응답 |
| `error` | Daemon → Client | 요청 실패 응답 |

### Task 관련 동작

- `create_task`는 `title`, `description`, `assignee`, `start_mode`, `priority`, `stale_after_seconds`를 받아 `pending` 상태 task를 만들고 task store / snapshot에 persist한다. inbox message enqueue, `Task ID:` 주입, wake, session ensure는 하지 않는다.
- MCP `start_task`는 새 task를 만들고 `Task ID:`를 dispatch 메시지에 자동 주입한 뒤, 메시지를 enqueue하고 대상 워크스페이스를 wake 한다. `start_mode="fresh"`이면 wake 전에 세션 재시작까지 포함한다.
- `update_task`는 assignee 또는 creator가 로그를 남길 수 있고, 상태 변경과 `result` 설정은 assignee가 담당한다. 상태 전이는 `pending → in_progress → completed|failed` 단방향이다.
- `get_task`와 `list_tasks` 응답은 단순 저장본이 아니라 daemon이 계산한 `stale_info`를 포함한다. 여기에는 pending message 수, 마지막 관련 메시지 시각, wake 재시도 상태, task/message divergence 정보가 들어간다.
- `cancel_task`, `remove_task`, `intervene_task`는 각각 취소, 아카이브, bounded recovery(`wake`/`interrupt`/`retry`)를 daemon control path로 수행한다.

### 연결 생명주기

```
1. MCP 서버가 Unix 소켓에 연결
2. register 메시지로 워크스페이스 이름 등록
3. 양방향 메시지 교환
4. 연결 종료 시 자동 unregister
```

### 메시지 큐와 wake

- 워크스페이스별 독립 큐
- `send_message`로 대상 큐에 enqueue
- `read_messages`로 자신의 큐에서 dequeue (소비 후 삭제)
- `read_messages`는 message queue만 소비하므로, 빈 결과가 assigned pending task 부재를 의미하지 않는다. task work는 `list_tasks` 또는 `list_workspace_tasks`로 별도 조회한다.
- 대상 워크스페이스가 연결 중이면 `push_message`로 즉시 알림
- MCP `send_message`/`request` 도구는 tmux로 즉시 wake를 시도하고, daemon의 `WakeScheduler`는 unread 메시지가 남아 있으면 idle 상태를 기다리며 backoff 재시도를 이어간다.

### 히스토리

- `~/.local/state/ax/` 디렉터리에 JSONL 형식으로 저장
- 최근 500건 유지

---

## MCP 도구

에이전트가 사용할 수 있는 MCP 도구 목록이다. `crates/ax-mcp-server/src/server.rs`에서 등록된다.

### 통신 도구

| 도구 | 파라미터 | 설명 |
|------|----------|------|
| `send_message` | `to` (필수), `message` (필수) | 대상 워크스페이스에 메시지 전송 + 자동 웨이크 |
| `read_messages` | `limit`, `from` | 대기 중인 inbox 메시지 읽기. task store 조회가 아님 |
| `broadcast_message` | `message` (필수) | 모든 워크스페이스에 브로드캐스트 |
| `request` | `to` (필수), `message` (필수), `timeout` | 동기 요청-응답 (전송 → 웨이크 → 폴링 대기) |

### 조회 도구

| 도구 | 파라미터 | 설명 |
|------|----------|------|
| `list_agents` | `query`, `active_only` | 설정된 에이전트 목록. launch mode, 활성 여부, `state`(`offline`/`idle`/`running`), `status_text` 포함 |
| `inspect_agent` | `name` (필수), `question`, `timeout` | 에이전트에 상태 질의 후 응답 대기 |
| `list_workspaces` | - | 활성 워크스페이스 목록 |

### 상태 도구

| 도구 | 파라미터 | 설명 |
|------|----------|------|
| `set_status` | `status` (필수) | 자신의 상태 텍스트 갱신 |

### 세션 제어 도구

| 도구 | 파라미터 | 설명 |
|------|----------|------|
| `interrupt_agent` | `name` (필수) | 대상 에이전트에 Escape 전송 |
| `send_keys` | `workspace` (필수), `keys` (필수 배열) | tmux 세션에 raw/special key 시퀀스를 주입. resuming prompt, yes/no 확인창, 입력 대기 해소에 사용 |

### Task 도구

| 도구 | 파라미터 | 설명 |
|------|----------|------|
| `create_task` | `title` (필수), `description`, `assignee` (필수), `start_mode`, `priority`, `stale_after_seconds` | pending task record만 생성. inbox message / wake / `Task ID:` 주입 없음 |
| `start_task` | `title` (필수), `message` (필수), `description`, `assignee` (필수), `start_mode`, `priority`, `stale_after_seconds` | task 생성 + `Task ID:` 자동 주입 dispatch + wake |
| `update_task` | `id` (필수), `status`, `result`, `log` | task 상태/결과 갱신 또는 progress log 추가 |
| `get_task` | `id` (필수) | 단일 task 상세 조회 |
| `list_tasks` | `assignee`, `created_by`, `status` | 조건별 task 목록 조회 |
| `list_workspace_tasks` | `workspace`, `view`, `status` | workspace 기준 assigned / created / both task 조회 |
| `cancel_task` | `id` (필수), `reason`, `expected_version` | active task 취소 |
| `remove_task` | `id` (필수), `reason`, `expected_version` | terminal task 보관/숨김 |
| `intervene_task` | `id` (필수), `action` (필수), `note`, `expected_version` | stuck task에 `wake`/`interrupt`/`retry` 적용 |

### 공유 저장소 도구

| 도구 | 파라미터 | 설명 |
|------|----------|------|
| `set_shared_value` | `key` (필수), `value` (필수) | 전역 키-값 저장 |
| `get_shared_value` | `key` (필수) | 키-값 조회 |
| `list_shared_values` | - | 모든 공유 값 목록 |

### 에이전트 웨이크 메커니즘

메시지 전송 시 대상 에이전트를 자동으로 깨운다:

```go
// tmux send-keys로 프롬프트 주입
// 1. Escape + C-u 로 현재 입력 클리어
// 2. 프롬프트 텍스트 입력
// 3. Enter 전송
```

즉시 wake 이후에도 unread 메시지가 남아 있으면 daemon의 `WakeScheduler`가 `5s → 10s → 20s → 40s → 60s` backoff로 최대 10회까지 재시도한다. 재시도는 tmux 세션이 존재하고 에이전트가 idle로 보일 때만 수행된다.

### TaskStore / WakeScheduler

- `TaskStore` (`crates/ax-daemon/src/task_store.rs`)는 daemon 상태 디렉터리의 `tasks.json`에 task를 저장한다. `create`/`update`/`refresh`는 변경 사항을 즉시 persist하고, `get`/`list`/`snapshot`은 방어적 복사본을 반환한다.
- `TaskStore::update`는 assignee/creator 권한, monotonic status transition, assignee 전용 `result` 쓰기, 중복 no-op 로그 억제를 함께 검증한다.
- `WakeScheduler` (`crates/ax-daemon/src/wake_scheduler.rs`)는 workspace별 pending wake를 추적한다. `send_message`가 메시지를 enqueue하면 scheduler entry가 등록되고, `read_messages`로 inbox가 비워지면 cancel된다.
- `WakeScheduler::state`는 top/diagnostics에서 볼 수 있는 현재 wake 재시도 상태(`sender`, `attempts`, `next_retry`)를 노출한다.

---

## 에이전트 런타임

### Runtime enum (`crates/ax-agent/src/runtime.rs`)

```rust
pub enum Runtime { Claude, Codex }

impl Runtime {
    pub fn as_str(self) -> &'static str;           // "claude" | "codex"
    pub fn normalize(name: &str) -> Option<Self>;  // 입력 문자열 정규화
    pub fn instruction_file(self) -> &'static str; // "CLAUDE.md" | "AGENTS.md"
}
```

런타임 실행/세션 부트스트랩은 `crates/ax-agent/src/launch.rs`의 `run_with_options`가 담당하며,
Codex 전용 `CODEX_HOME` 격리는 `crates/ax-agent/src/codex.rs`에서 관리한다.
`agent_providers` / `agent_provider` 설정은 runtime 선택과 분리되어 있으며, Codex runtime이면 managed `CODEX_HOME/config.toml`에 `model_provider`, `model`, `[model_providers.<id>]` (`base_url`, `env_key`, `wire_api`)와 local endpoint용 `web_search`로 반영된다.

### 지원 런타임

| 런타임 | 지시 파일 | CLI 명령 | 특이 사항 |
|--------|-----------|----------|-----------|
| `claude` | `CLAUDE.md` | `claude --dangerously-skip-permissions` | `--continue` 플래그로 세션 유지 시도, 실패 시 fallback |
| `codex` | `AGENTS.md` | `codex --dangerously-bypass-approvals-and-sandbox` | `CODEX_HOME` 환경변수로 격리 |

### 워크스페이스 실행 모드

설정의 `agent` 필드에 따라 실행 모드가 결정된다:

| `agent` 값 | 모드 | 동작 |
|-------------|------|------|
| (미설정) | `runtime` | `ax run-agent`를 통해 런타임(claude/codex) 자동 실행 |
| `"none"` | `manual` | 셸만 실행, 에이전트 미기동 |
| `"custom-cmd"` | `custom` | 지정된 커스텀 명령 실행 |

### 새 런타임 추가 방법

1. `crates/ax-agent/src/` 에 새 런타임 구현 파일 추가
2. `Runtime` enum에 variant 추가 + `as_str`/`normalize`/`instruction_file` 케이스 등록
3. `SUPPORTED_RUNTIMES` 배열에 이름 추가
4. `crates/ax-agent/src/launch.rs`의 실행 경로에서 새 런타임을 디스패치하도록 갱신

---

## 계층적 프로젝트 구조

ax는 중첩된 프로젝트 구조를 지원한다.

### 구조 예시

```
monorepo/
├── .ax/config.yaml          # 루트 프로젝트
├── services/
│   ├── api/
│   │   └── .ax/config.yaml  # 자식 프로젝트 "api"
│   └── web/
│       └── .ax/config.yaml  # 자식 프로젝트 "web"
```

```yaml
# monorepo/.ax/config.yaml
project: monorepo
workspaces:
  infra:
    dir: ./infra
    description: 인프라 관리
children:
  api:
    dir: ./services/api
  web:
    dir: ./services/web
```

### 워크스페이스 이름 병합

자식 프로젝트의 워크스페이스는 `{prefix}.{name}` 형태로 병합된다:

```
monorepo 워크스페이스:
  - infra              (루트 직접 관리)
  - api.main           (api 프로젝트 워크스페이스)
  - api.worker         (api 프로젝트 워크스페이스)
  - web.frontend       (web 프로젝트 워크스페이스)
```

### 오케스트레이터 트리

```
orchestrator (루트)
├── 직접 관리: infra
├── api.orchestrator (서브)
│   └── 직접 관리: api.main, api.worker
└── web.orchestrator (서브)
    └── 직접 관리: web.frontend
```

### ProjectNode (`crates/ax-config/src/tree.rs`)

`Config::load_tree()`는 계층 구조를 보존한 트리를 반환한다:

```rust
pub struct ProjectNode {
    pub name: String,
    pub alias: String,                       // 부모 children 맵의 마운트 별칭
    pub prefix: String,                      // 병합 시 사용할 완전 접두사(e.g. "team.sub")
    pub dir: PathBuf,
    pub orchestrator_runtime: String,
    pub disable_root_orchestrator: bool,
    pub workspaces: Vec<WorkspaceRef>,       // 이 프로젝트의 워크스페이스
    pub children: Vec<ProjectNode>,          // 자식 프로젝트
}
```

---

## 릴리스

### 버전 관리

Git 태그 기반 시맨틱 버전:

```bash
make release patch   # v0.0.0 → v0.0.1
make release minor   # v0.0.1 → v0.1.0
make release major   # v0.1.0 → v1.0.0
make release dev     # v0.1.0 → v0.1.1-dev1
```

### CI/CD 파이프라인

1. `make release <type>` → git 태그 생성 → origin에 push
2. GitHub Actions 트리거 (`v*` 태그 매칭)
3. GoReleaser가 멀티 플랫폼 바이너리 빌드
4. GitHub Releases에 자동 게시

### 지원 플랫폼

| OS | 아키텍처 |
|----|----------|
| Linux | amd64, arm64 |
| macOS (Darwin) | amd64, arm64 |

---

## 코드 흐름 상세

### `ax up` 실행 흐름

```
ax up
  │
  ├─ 1. resolveConfigPath()
  │     └─ config.FindConfigFile(): 상위 디렉터리 탐색
  │
  ├─ 2. config.Load(path)
  │     └─ loadRecursive(): 자식 재귀 로딩 + 워크스페이스 병합
  │
  ├─ 3. daemon.Start()
  │     └─ 백그라운드 프로세스로 ax daemon start 실행
  │
  ├─ 4. workspace.Manager.CreateAll(cfg)
  │     └─ 각 워크스페이스마다:
  │         ├─ WriteMCPConfig(): .mcp.json 생성 (ax MCP 서버 등록)
  │         ├─ WriteInstructions(): CLAUDE.md/AGENTS.md 작성
  │         └─ tmux.CreateSessionWithArgs(): tmux 세션 + ax run-agent
  │
  └─ 5. ensureOrchestrators(tree)
        └─ 프로젝트 트리 순회:
            ├─ 오케스트레이터 디렉터리 생성
            ├─ WriteOrchestratorPrompt(): 역할별 프롬프트 자동 생성
            ├─ WriteMCPConfig(): 오케스트레이터 MCP 설정
            └─ tmux.CreateSessionWithArgs(): 오케스트레이터 세션
```

### 에이전트 간 메시지 전달 흐름

```
에이전트 A (claude)
  │
  │ MCP tool call: send_message(to="B", message="...")
  │
  ▼
MCP Server A ──[Unix socket]──▶ Daemon
  │                               │
  │                               ├─ queue.Enqueue("A", "B", msg)
  │                               ├─ history.Append("A", "B", msg)
  │                               ├─ wakeScheduler.Schedule("B", "A")
  │                               └─ push_message → MCP Server B (연결 중이면)
  │
  ├─ wakeAgent("B", "A")
  │   └─ tmux send-keys: "read_messages로 메시지 확인..."
  │
  └─ daemon retry
      └─ idle 상태가 될 때까지 WakeScheduler가 backoff 재시도
  ▼
에이전트 B (claude)
  │
  │ MCP tool call: read_messages()
  │
  ▼
MCP Server B ──[Unix socket]──▶ Daemon
                                  │
                                  └─ queue.Dequeue("B", limit, from)
                                       └─ 메시지 반환
```

### 워크스페이스 생성 시 파일 변경

```
workspace 디렉터리/
├── .mcp.json          # MCP 서버 설정 (ax 엔트리 추가/병합)
├── CLAUDE.md          # (claude 런타임) 에이전트 지시사항
└── AGENTS.md          # (codex 런타임) 에이전트 지시사항
```

`.mcp.json` 형식:

```json
{
  "mcpServers": {
    "ax": {
      "command": "/path/to/ax",
      "args": ["mcp-server", "--workspace", "name", "--socket", "..."]
    }
  }
}
```

---

## 의존성

### 직접 의존성

| 패키지 | 버전 | 용도 |
|--------|------|------|
| `github.com/spf13/cobra` | v1.10.2 | CLI 프레임워크 |
| `github.com/charmbracelet/bubbletea` | v1.3.10 | 터미널 UI |
| `github.com/charmbracelet/lipgloss` | v1.1.0 | 터미널 스타일링 |
| `github.com/mark3labs/mcp-go` | v0.47.1 | MCP 프로토콜 구현 |
| `github.com/google/uuid` | v1.6.0 | UUID 생성 |
| `gopkg.in/yaml.v3` | v3.0.1 | YAML 파싱 |
