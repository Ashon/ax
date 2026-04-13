# ax Developer Guide

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
- MCP 표준 도구 인터페이스로 에이전트 간 통신
- 계층적 프로젝트 트리와 서브 오케스트레이터 지원
- BubbleTea 기반 TUI (shell, watch)

---

## 아키텍처

```
┌─────────────────────────────────────────────────────────────┐
│                         사용자                               │
│                    ax shell / ax watch                       │
└──────────────┬──────────────────────────────┬───────────────┘
               │                              │
               ▼                              ▼
┌──────────────────────┐     ┌────────────────────────────────┐
│   orchestrator       │     │         ax daemon              │
│   (tmux session)     │     │    (Unix socket server)        │
│                      │     │                                │
│  ┌────────────────┐  │     │  ┌──────────┐ ┌────────────┐  │
│  │  claude/codex  │  │     │  │ Registry │ │ MessageQueue│  │
│  │  CLI agent     │  │     │  └──────────┘ └────────────┘  │
│  └───────┬────────┘  │     │  ┌──────────┐ ┌────────────┐  │
│          │           │     │  │ History  │ │SharedValues │  │
│  ┌───────┴────────┐  │     │  └──────────┘ └────────────┘  │
│  │  MCP server    │──╋────▶│                                │
│  │  (ax mcp-server)│ │     └────────────────────────────────┘
│  └────────────────┘  │                    ▲
└──────────────────────┘                    │
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
| **ax daemon** | Unix 소켓 기반 중앙 메시지 브로커. 워크스페이스 등록/해제, 메시지 큐, 공유 값 저장소 |
| **MCP server** | 각 워크스페이스에 stdio로 연결되는 MCP 서버. 에이전트가 사용할 도구 노출 |
| **Workspace** | 하나의 tmux 세션 + 에이전트 런타임(claude/codex). 격리된 작업 환경 |
| **Orchestrator** | 다른 워크스페이스들을 조율하는 특수 에이전트. 자동 생성된 프롬프트로 역할 부여 |

---

## 프로젝트 구조

```
ax/
├── main.go                          # 엔트리포인트 → cmd.Execute()
├── go.mod                           # Go 1.26.2, 의존성 정의
├── Makefile                         # 빌드/테스트/릴리스 타겟
├── .goreleaser.yaml                 # 멀티 플랫폼 바이너리 빌드 설정
├── .github/workflows/release.yaml   # GitHub Actions 릴리스 파이프라인
│
├── cmd/                             # CLI 명령어 (cobra)
│   ├── root.go                      #   루트 커맨드, 글로벌 플래그
│   ├── init_cmd.go                  #   프로젝트 초기화 (인터랙티브)
│   ├── up.go                        #   데몬 + 워크스페이스 기동
│   ├── down.go                      #   워크스페이스 + 데몬 종료
│   ├── daemon.go                    #   데몬 start/stop/status
│   ├── workspace.go                 #   워크스페이스 create/destroy/list/attach
│   ├── shell.go, shell_tui.go       #   오케스트레이터 대화 TUI
│   ├── watch.go                     #   워크스페이스 모니터링 TUI
│   ├── status.go                    #   프로젝트 상태 표시
│   ├── send.go                      #   메시지 전송 + 에이전트 웨이크
│   ├── orchestrators.go             #   오케스트레이터 세션 보장
│   ├── messages.go                  #   메시지 관리 헬퍼
│   ├── run_agent.go                 #   에이전트 실행 (tmux 내부에서 호출)
│   └── mcpserver.go                 #   MCP 서버 래퍼
│
└── internal/                        # 핵심 패키지
    ├── config/                      #   YAML 설정 로딩/병합
    │   ├── config.go                #     Config 구조체, 재귀적 자식 병합
    │   ├── tree.go                  #     ProjectNode 계층 트리
    │   └── config_test.go           #     중첩 로딩/순환 참조 테스트
    │
    ├── daemon/                      #   메시지 브로커
    │   ├── daemon.go                #     Unix 소켓 리스너, 연결 핸들러
    │   ├── protocol.go              #     메시지 엔벨로프 타입 정의
    │   ├── registry.go              #     워크스페이스 등록 관리
    │   ├── msgqueue.go              #     워크스페이스별 메시지 큐
    │   ├── history.go               #     JSONL 메시지 히스토리
    │   └── daemon_test.go           #     프로토콜/레지스트리/큐 테스트
    │
    ├── mcpserver/                   #   MCP 프로토콜 서버
    │   ├── server.go                #     MCP 서버 설정 및 도구 등록
    │   ├── tools.go                 #     12개 MCP 도구 구현
    │   └── client.go                #     데몬 연결 클라이언트
    │
    ├── agent/                       #   에이전트 런타임 추상화
    │   ├── runtime.go               #     Runtime 인터페이스, 디스패처
    │   ├── claude.go                #     Claude CLI 통합
    │   ├── codex.go                 #     Codex 에이전트 통합
    │   ├── codex_home.go            #     Codex 환경 설정
    │   └── shell.go                 #     셸 유틸리티
    │
    ├── workspace/                   #   워크스페이스 관리
    │   ├── workspace.go             #     Manager: Create/Destroy/CreateAll
    │   ├── mcpconfig.go             #     .mcp.json 생성/삭제
    │   ├── orchestrator.go          #     오케스트레이터 프롬프트 생성
    │   └── instructions.go          #     에이전트 지시 파일 관리
    │
    ├── tmux/                        #   tmux 세션 관리
    │   └── tmux.go                  #     세션 생성/삭제/attach/list/키 전송
    │
    └── types/                       #   공유 데이터 타입
        └── types.go                 #     AgentStatus, WorkspaceInfo, Message
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

다른 워크스페이스들을 관리하는 특수한 워크스페이스이다.

- `ax up` 시 프로젝트 트리를 순회하며 자동 생성
- 역할별 프롬프트가 자동으로 작성됨 (한국어)
- **루트 오케스트레이터**: 사용자 요청을 받아 워크스페이스/서브 오케스트레이터에 분배
- **서브 오케스트레이터**: 자체 프로젝트 범위의 워크스페이스를 관리, 상위에 보고

### Daemon

모든 워크스페이스 간 통신을 중개하는 서버이다.

- Unix 도메인 소켓(`~/.local/state/ax/daemon.sock`)으로 통신
- 뉴라인 구분 JSON 프로토콜
- 워크스페이스 레지스트리, 메시지 큐, 공유 값 저장소 관리
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
| **Go** | 1.26.2+ | 빌드/테스트 |
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
./ax up         # 데몬 + 워크스페이스 기동
./ax shell      # 오케스트레이터와 대화
./ax watch      # 워크스페이스 모니터링
./ax down       # 종료
```

---

## 빌드 및 테스트

### Makefile 타겟

```bash
make build      # 바이너리 빌드 (ldflags로 버전 주입)
make test       # go test ./... 실행
make clean      # 바이너리 삭제
make snapshot   # goreleaser 스냅샷 (멀티 플랫폼)
```

### 빌드 플래그

```bash
go build -ldflags "-s -w -X github.com/ashon/ax/cmd.version=$(VERSION)" -o ax .
```

- `-s -w`: 심볼/디버그 정보 제거 (바이너리 크기 축소)
- `-X`: `cmd.version` 변수에 버전 문자열 주입

### 테스트

```bash
go test ./...                          # 전체 테스트
go test ./internal/config/...          # config 패키지만
go test ./internal/daemon/...          # daemon 패키지만
go test -v -run TestLoadMerges ./...   # 특정 테스트
```

테스트 커버리지:

- `internal/config/config_test.go`: 재귀적 자식 로딩, 순환 참조 방지
- `internal/daemon/daemon_test.go`: 프로토콜 직렬화, 레지스트리, 메시지 큐, 공유 값

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

workspaces:
  frontend:
    dir: ./frontend                  # 작업 디렉터리 (상대/절대/~ 경로)
    description: "React 프론트엔드"   # 에이전트 설명
    runtime: claude                  # claude 또는 codex (기본: claude)
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

### Config 구조체 (`internal/config/config.go`)

```go
type Config struct {
    Project             string               `yaml:"project"`
    OrchestratorRuntime string               `yaml:"orchestrator_runtime,omitempty"`
    Children            map[string]Child     `yaml:"children,omitempty"`
    Workspaces          map[string]Workspace `yaml:"workspaces"`
}

type Workspace struct {
    Dir          string            `yaml:"dir"`
    Description  string            `yaml:"description,omitempty"`
    Shell        string            `yaml:"shell,omitempty"`
    Runtime      string            `yaml:"runtime,omitempty"`
    Agent        string            `yaml:"agent,omitempty"`
    Instructions string            `yaml:"instructions,omitempty"`
    Env          map[string]string `yaml:"env,omitempty"`
}

type Child struct {
    Dir    string `yaml:"dir"`
    Prefix string `yaml:"prefix,omitempty"`
}
```

### 설정 로딩 과정

1. `config.FindConfigFile()`: 현재 디렉터리부터 위로 올라가며 가장 상위 조상의 config를 찾음
2. `config.Load(path)`: 재귀적으로 자식 config를 로드하여 워크스페이스를 병합
   - 자식 워크스페이스 이름은 `{prefix}.{name}` 형태로 병합
   - 순환 참조 감지 시 에러 반환
   - 누락된 자식은 경고 후 스킵
3. `config.LoadTree(path)`: 병합 대신 계층 구조를 보존한 `ProjectNode` 트리 반환

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
| `ax up` | 데몬 시작 → 워크스페이스 생성 → 오케스트레이터 보장 |
| `ax down` | 모든 워크스페이스 종료 → 데몬 정지 |
| `ax status` | 데몬/워크스페이스 상태, 프로젝트 트리 표시 |
| `ax shell` | 루트 오케스트레이터와 대화하는 TUI |
| `ax watch` | 워크스페이스 실시간 모니터링 TUI |
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
| `read_messages` | Client → Daemon | 대기 중인 메시지 읽기 |
| `list_workspaces` | Client → Daemon | 활성 워크스페이스 목록 |
| `set_status` | Client → Daemon | 워크스페이스 상태 텍스트 갱신 |
| `set_shared` | Client → Daemon | 공유 키-값 저장 |
| `get_shared` | Client → Daemon | 공유 키-값 조회 |
| `list_shared` | Client → Daemon | 모든 공유 값 목록 |
| `push_message` | Daemon → Client | 새 메시지 푸시 알림 |
| `response` | Daemon → Client | 요청 성공 응답 |
| `error` | Daemon → Client | 요청 실패 응답 |

### 연결 생명주기

```
1. MCP 서버가 Unix 소켓에 연결
2. register 메시지로 워크스페이스 이름 등록
3. 양방향 메시지 교환
4. 연결 종료 시 자동 unregister
```

### 메시지 큐

- 워크스페이스별 독립 큐
- `send_message`로 대상 큐에 enqueue
- `read_messages`로 자신의 큐에서 dequeue (소비 후 삭제)
- 대상 워크스페이스가 연결 중이면 `push_message`로 즉시 알림

### 히스토리

- `~/.local/state/ax/` 디렉터리에 JSONL 형식으로 저장
- 최근 500건 유지

---

## MCP 도구

에이전트가 사용할 수 있는 MCP 도구 목록이다. `internal/mcpserver/tools.go`에서 등록된다.

### 통신 도구

| 도구 | 파라미터 | 설명 |
|------|----------|------|
| `send_message` | `to` (필수), `message` (필수) | 대상 워크스페이스에 메시지 전송 + 자동 웨이크 |
| `read_messages` | `limit`, `from` | 대기 중인 메시지 읽기 |
| `broadcast_message` | `message` (필수) | 모든 워크스페이스에 브로드캐스트 |
| `request` | `to` (필수), `message` (필수), `timeout` | 동기 요청-응답 (전송 → 웨이크 → 폴링 대기) |

### 조회 도구

| 도구 | 파라미터 | 설명 |
|------|----------|------|
| `list_agents` | `query`, `active_only` | 설정된 에이전트 목록 (활성 상태 포함) |
| `inspect_agent` | `name` (필수), `question`, `timeout` | 에이전트에 상태 질의 후 응답 대기 |
| `list_workspaces` | - | 활성 워크스페이스 목록 |

### 상태 도구

| 도구 | 파라미터 | 설명 |
|------|----------|------|
| `set_status` | `status` (필수) | 자신의 상태 텍스트 갱신 |
| `interrupt_agent` | `name` (필수) | 대상 에이전트에 Escape 전송 |

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

---

## 에이전트 런타임

### Runtime 인터페이스 (`internal/agent/runtime.go`)

```go
type Runtime interface {
    Name() string
    InstructionFile() string
    Launch(dir, workspace, socketPath, axBin, configPath string) error
    UserCommand(dir, workspace, socketPath, axBin, configPath string) (string, error)
}
```

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

1. `internal/agent/` 에 새 런타임 구현 파일 추가
2. `Runtime` 인터페이스 구현
3. `runtime.go`의 `Get()` 함수에 케이스 추가
4. `supportedRuntimeNames` 슬라이스에 이름 추가

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

### ProjectNode (`internal/config/tree.go`)

`LoadTree()`는 계층 구조를 보존한 트리를 반환한다:

```go
type ProjectNode struct {
    Name                string
    Prefix              string          // 완전한 접두사
    Dir                 string
    OrchestratorRuntime string
    Workspaces          []WorkspaceRef  // 이 프로젝트의 워크스페이스
    Children            []*ProjectNode  // 자식 프로젝트
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
  │                               └─ push_message → MCP Server B (연결 중이면)
  │
  ├─ wakeAgent("B", "A")
  │   └─ tmux send-keys: "read_messages로 메시지 확인..."
  │
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
