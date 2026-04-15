# ax

tmux 기반 멀티 에이전트 LLM 워크스페이스 매니저

여러 LLM 에이전트(Claude, Codex)를 격리된 tmux 세션에서 동시에 실행하고, MCP(Model Context Protocol)를 통해 에이전트 간 통신과 협업을 가능하게 합니다.

```
┌─────────────────────────────────────────────┐
│           ax claude / ax codex               │
│        (foreground orchestrator CLI)         │
└──────────────────┬──────────────────────────┘
                   │
         ┌─────────┼─────────┐
         ▼         ▼         ▼
    ┌─────────┐ ┌─────────┐ ┌─────────┐
    │frontend │ │backend  │ │  infra  │
    │ claude  │ │ claude  │ │  codex  │
    └────┬────┘ └────┬────┘ └────┬────┘
         │           │           │
         └─────────┬─┘───────────┘
                   ▼
            ┌────────────┐
            │  ax daemon │
            │ (message   │
            │  broker)   │
            └────────────┘
```

## 설치

### 요구 사항

- **tmux** 3.x 이상
- **claude** CLI (Claude 런타임 사용 시)
- **codex** CLI (Codex 런타임 사용 시, 선택)

### GitHub Releases에서 다운로드

[Releases](https://github.com/Ashon/ax/releases) 페이지에서 플랫폼에 맞는 바이너리를 다운로드합니다.

```bash
# macOS (Apple Silicon)
curl -Lo ax.tar.gz https://github.com/Ashon/ax/releases/latest/download/ax_Darwin_arm64.tar.gz
tar xzf ax.tar.gz
sudo mv ax /usr/local/bin/

# macOS (Intel)
curl -Lo ax.tar.gz https://github.com/Ashon/ax/releases/latest/download/ax_Darwin_amd64.tar.gz
tar xzf ax.tar.gz
sudo mv ax /usr/local/bin/

# Linux (amd64)
curl -Lo ax.tar.gz https://github.com/Ashon/ax/releases/latest/download/ax_Linux_amd64.tar.gz
tar xzf ax.tar.gz
sudo mv ax /usr/local/bin/

# Linux (arm64)
curl -Lo ax.tar.gz https://github.com/Ashon/ax/releases/latest/download/ax_Linux_arm64.tar.gz
tar xzf ax.tar.gz
sudo mv ax /usr/local/bin/
```

### go install

Go 1.26.2 이상이 설치되어 있다면 한 줄로 설치할 수 있습니다.

```bash
go install github.com/ashon/ax@latest
```

`$GOPATH/bin`(기본값 `~/go/bin`)이 `PATH`에 포함되어 있어야 합니다.

### 소스에서 설치

```bash
git clone https://github.com/Ashon/ax.git
cd ax

# make build + copy to $(go env GOPATH)/bin/ax
make install
```

`make install`은 빌드 후 바이너리를 `$(go env GOPATH)/bin/ax`에 복사하고, 이어서 `codesign -s - $(go env GOPATH)/bin/ax`를 실행합니다.

- macOS에서는 ad-hoc codesign까지 포함한 기본 설치 경로입니다.
- `$(go env GOPATH)/bin`(보통 `~/go/bin`)이 `PATH`에 포함되어 있어야 합니다.
- `codesign`이 없는 환경(예: 일반적인 Linux)에서는 수동 설치를 사용하세요.

```bash
make build
sudo mv ax /usr/local/bin/
```

## 빠른 시작

### 1. 프로젝트 초기화

```bash
cd /path/to/your/project
ax init
ax init --codex
```

기본값은 `claude` setup agent입니다. `ax init --codex`를 사용하면 setup agent와 생성되는 기본 워크스페이스 런타임을 `codex`로 맞춥니다.
`--claude`로 기본값을 명시할 수 있고, `--no-setup` 플래그로 수동 설정도 가능합니다.

### 2. 워크스페이스 기동

```bash
ax up
```

데몬을 시작하고, 설정된 모든 워크스페이스와 오케스트레이터를 생성합니다.

### 3. 오케스트레이터와 대화

```bash
ax claude   # 또는 ax codex
```

현재 터미널에서 코딩 에이전트 CLI(`claude` 또는 `codex`)를 포그라운드로 실행합니다.
실행된 CLI는 루트 오케스트레이터의 프롬프트와 MCP 설정을 그대로 상속받아 ax 데몬에 `orchestrator` 정체성으로 등록되며, 요청을 분석해 적절한 워크스페이스 / 서브 오케스트레이터에 분배합니다.
CLI를 종료하면 루트 오케스트레이터 세션도 함께 종료됩니다. 서브 오케스트레이터와 워크스페이스는 계속 실행됩니다.

### 4. 워크스페이스 모니터링

```bash
ax top
```

모든 워크스페이스의 상태, 토큰 사용량, 활동을 실시간으로 확인합니다.

### 5. 종료

```bash
ax down
```

모든 워크스페이스, 오케스트레이터, 데몬을 정리합니다.

## 설정

설정 파일은 `.ax/config.yaml`에 위치합니다.

```yaml
project: my-project
orchestrator_runtime: claude

workspaces:
  frontend:
    dir: ./frontend
    description: "React 프론트엔드 개발"
    runtime: claude
    instructions: |
      React와 TypeScript를 사용합니다.
      테스트는 vitest로 실행합니다.

  backend:
    dir: ./backend
    description: "Go API 서버 개발"
    runtime: claude
    instructions: |
      Go 표준 라이브러리 중심으로 작성합니다.

  infra:
    dir: ./infra
    description: "인프라 관리"
    runtime: codex
```

### 워크스페이스 옵션

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `dir` | 작업 디렉터리 (상대/절대/`~` 경로) | `.` |
| `description` | 에이전트 역할 설명 | - |
| `runtime` | `claude` 또는 `codex` | `claude` |
| `orchestrator_runtime` | 루트/서브 오케스트레이터 런타임 | `claude` |
| `instructions` | 에이전트에게 전달할 지시사항 | - |
| `agent` | 커스텀 에이전트 명령 (`"none"`이면 에이전트 미실행) | - |
| `shell` | `agent: none` 시 사용할 셸 | - |
| `env` | 환경 변수 맵 | - |

### 계층적 프로젝트 (모노레포)

자식 프로젝트를 `children`으로 연결하면, 각 프로젝트에 서브 오케스트레이터가 자동 생성됩니다.

```yaml
# 루트 .ax/config.yaml
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

자식 프로젝트의 워크스페이스는 `{prefix}.{name}` 형태로 병합됩니다 (예: `api.main`, `web.frontend`).

자식 프로젝트 디렉터리에서 `ax init`을 실행하면 부모 config에 자동 등록됩니다.

### 글로벌 설정

홈 디렉터리에 전역 config를 만들어 최상위 오케스트레이터로 사용할 수 있습니다.

```bash
ax init --global
```

## CLI 레퍼런스

### top-level 명령어 (`ax --help`)

```bash
ax claude [claude args...]       # 루트 오케스트레이터로 Claude CLI 실행 (포그라운드)
ax codex [codex args...]         # 루트 오케스트레이터로 Codex CLI 실행 (포그라운드)
ax completion <shell>            # 셸 자동완성 스크립트 생성
ax daemon                        # 데몬 관리
ax down                          # 전체 종료
ax init                          # 프로젝트 초기화 (기본: setup agent)
ax messages                      # CLI inbox(_cli) 메시지 조회
ax refresh                       # 생성된 ax 파일 리프레시 및 세션 reconcile
ax send <workspace> <message>    # 메시지 전송 + 에이전트 wake
ax status                        # 전체 상태 표시
ax tasks                         # task 상태/진행 조회
ax up                            # 데몬 + 워크스페이스 기동
ax top                           # 워크스페이스 모니터링 TUI
ax workspace                     # 워크스페이스 관리
```

숨김 내부 명령으로 `ax run-agent`, `ax mcp-server`, deprecated `ax messages-json`가 있습니다.

런타임 인자를 함께 넘길 때는 `ax` 전역 플래그를 서브커맨드 앞에 둡니다. 예: `ax --config .ax/config.yaml codex resume --last`

### 워크스페이스/작업/메시지 관련 하위 명령

```bash
ax workspace list                # 활성 워크스페이스 목록
ax workspace attach <name>       # tmux 세션에 연결
ax workspace create <name>       # 워크스페이스 수동 생성
ax workspace destroy <name>      # 워크스페이스 삭제
ax workspace interrupt <name>    # 에이전트에 Escape 전송
ax tasks show <task-id>          # task 상세, 로그, 관련 메시지 표시
ax tasks activity [task-id]      # task activity 타임라인 표시
ax messages --json               # CLI inbox 메시지를 JSON으로 출력
ax messages --wait               # CLI inbox 메시지 대기
```

### 설정 산출물 리프레시

```bash
ax refresh                       # .mcp.json, AGENTS.md/CLAUDE.md, orchestrator 프롬프트 재생성
ax refresh --start-missing       # 꺼져 있는 configured session도 함께 시작
ax refresh --restart             # 실행 중 세션까지 재시작해서 런타임 변경 즉시 반영
```

### 데몬 관리

```bash
ax daemon start                  # 데몬 시작
ax daemon stop                   # 데몬 정지
ax daemon status                 # 상태 확인
```

### 전역 플래그

```bash
--socket <path>    # 데몬 소켓 경로 (기본: ~/.local/state/ax/daemon.sock)
--config <path>    # 설정 파일 경로 (기본: 자동 탐색)
```

## 동작 원리

### 아키텍처

1. **ax daemon**: Unix 소켓 기반 메시지 브로커. 워크스페이스 등록/해제, 메시지 큐, 공유 키-값 저장소 관리
2. **MCP server**: 각 워크스페이스에 stdio로 연결. 에이전트에게 통신 도구 제공
3. **Workspace**: 격리된 tmux 세션에서 에이전트(claude/codex) 실행
4. **Orchestrator**: 자동 생성된 프롬프트로 다른 워크스페이스들을 조율하는 특수 에이전트

### 에이전트 간 통신

에이전트들은 MCP 도구를 통해 서로 통신합니다:

- 조회/탐색: `list_agents`, `inspect_agent`, `list_workspaces`
- 메시징: `send_message`, `read_messages`, `broadcast_message`, `request`
- 상태/공유값: `set_status`, `set_shared_value`, `get_shared_value`, `list_shared_values`
- tmux 제어: `interrupt_agent`, `send_keys`
- 작업 관리: `create_task`, `update_task`, `get_task`, `list_tasks`

메시지 전송 시 대상 에이전트가 자동으로 wake 됩니다 (tmux 키 입력 주입).

## 개발

개발 관련 자세한 내용은 [DEVELOPER_GUIDE.md](DEVELOPER_GUIDE.md)를 참고하세요.

```bash
make build      # 바이너리 빌드
make test       # 테스트 실행
make snapshot   # 멀티 플랫폼 스냅샷 빌드
```

## License

MIT
