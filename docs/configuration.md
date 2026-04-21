# 설정 가이드

`ax` 설정의 기준 파일은 `.ax/config.yaml` 입니다.

## 기본 예시

```yaml
project: my-project
orchestrator_runtime: claude
idle_timeout_minutes: 15

workspaces:
  frontend:
    dir: ./frontend
    description: React 프론트엔드 개발
    runtime: claude
    instructions: |
      React와 TypeScript를 사용합니다.
      테스트는 vitest로 실행합니다.

  backend:
    dir: ./backend
    description: Go API 서버 개발
    runtime: codex

children:
  api:
    dir: ./services/api
  web:
    dir: ./services/web
```

## 루트 필드

| 필드 | 설명 |
|---|---|
| `project` | 프로젝트 이름 |
| `orchestrator_runtime` | root / child orchestrator 기본 runtime |
| `disable_root_orchestrator` | managed root orchestrator state 비활성화 |
| `experimental_mcp_team_reconfigure` | team reconfigure 실험 기능 |
| `codex_model_reasoning_effort` | Codex 기본 reasoning effort |
| `idle_timeout_minutes` | workspace idle sleep 기준 시간. 런타임이 daemon 등록 payload에 양수 timeout을 전달한 workspace만 auto sleep 후보가 됩니다. |
| `workspaces` | 현재 프로젝트의 workspace 정의 |
| `children` | 자식 프로젝트 정의 |

## workspace 필드

| 필드 | 설명 |
|---|---|
| `dir` | 작업 디렉터리 |
| `description` | 역할 설명 |
| `runtime` | `claude` 또는 `codex` |
| `codex_model_reasoning_effort` | workspace별 Codex reasoning effort override |
| `instructions` | workspace instruction body |
| `agent` | 커스텀 명령. `"none"`이면 runtime 대신 셸만 유지 |
| `shell` | `agent: none`일 때 사용할 셸 |
| `env` | 세션에 주입할 환경 변수 |

## on-demand lifecycle과 설정의 관계

현재 구현 기준:

- `ax up`은 artifact만 준비합니다.
- 실제 workspace session은 메시지 또는 task dispatch 시 시작됩니다.
- 양수 idle timeout이 지나고 queued work / wake retry / open assigned task / 최근 활동이 없으며 tmux session이 실제 idle이면 workspace는 auto sleep 될 수 있습니다.
- orchestrator(`orchestrator`, `*.orchestrator`)는 always-on 대상이라 auto sleep 대상에서 제외됩니다.

## 계층적 프로젝트

`children`을 연결하면 각 자식 프로젝트에 child orchestrator가 생깁니다.

예시:

```yaml
project: monorepo

workspaces:
  infra:
    dir: ./infra

children:
  api:
    dir: ./services/api
  web:
    dir: ./services/web
```

자식 프로젝트의 workspace는 prefix가 붙어 병합됩니다.

- `api.main`
- `web.frontend`
- `api.orchestrator`
- `web.orchestrator`

## 글로벌 설정

전역 root config를 만들고 싶다면:

```bash
ax init --global
```

이 경우 홈 디렉터리 아래 config가 tree root처럼 동작할 수 있습니다.

## artifact 반영

설정 변경 후 자주 쓰는 명령:

```bash
ax refresh
ax refresh --start-missing
ax refresh --restart
```

의미:

- `ax refresh`: instruction / MCP config / orchestrator prompt 재생성
- `--start-missing`: 현재 꺼진 configured session도 시작
- `--restart`: 이미 실행 중인 managed session도 재시작

주의:

- root orchestrator는 foreground/ephemeral이므로 hot reload 대상이 아닙니다.
- root prompt가 바뀌었다면 `ax claude` / `ax codex`를 다시 실행해야 합니다.
