# 아키텍처

`ax`는 CLI, daemon, MCP server, tmux session, runtime home/state가 서로 분리된 구조로 동작합니다.

## 구성 요소

### 1. CLI

사용자가 직접 실행하는 진입점입니다.

주요 역할:

- `ax init`: config 생성
- `ax up`: daemon + artifact 준비
- `ax claude` / `ax codex`: root orchestrator 실행
- `ax status` / `ax top`: 상태 관측
- `ax down`: 전체 정리

### 2. daemon

Unix socket 기반 중앙 브로커입니다.

주요 책임:

- workspace 등록 / 연결 상태 추적
- 메시지 큐와 history 저장
- task durable state 저장 및 snapshot 생성
- wake retry / stale recovery 보조
- session lifecycle policy 집약
- durable memory 저장

주요 state 파일:

| 파일 | 역할 |
|---|---|
| `daemon.sock` | daemon Unix socket |
| `daemon.pid` | daemon process 확인 |
| `queue.json` | pending message queue |
| `history.jsonl` | 메시지 history |
| `tasks-state.json` | durable task state |
| `tasks.json` | watch/status용 materialized task snapshot |
| `memories.json` | durable memory store |

### 3. MCP server

각 agent runtime에 stdio로 붙는 도구 서버입니다.

역할:

- daemon client 역할 수행
- message / task / memory / lifecycle / status 관련 도구 노출
- runtime가 다른 agent와 협업할 수 있도록 공통 인터페이스 제공

### 4. workspace session

하나의 workspace는 보통 다음 조합으로 동작합니다.

- tmux session
- `claude` 또는 `codex` runtime
- `.mcp.json`
- runtime instruction file (`CLAUDE.md`, `AGENTS.md`)

### 5. orchestrator

두 종류가 있습니다.

- root orchestrator: `ax claude` / `ax codex`로 foreground 실행
- child orchestrator: 프로젝트 트리 기준 managed session

현재 구현 기준:

- root orchestrator는 lifecycle tool로 직접 start/stop/restart 하는 managed 대상이 아님
- child orchestrator는 managed session이며 on-demand dispatch 시 시작 가능
- orchestrator 계열은 auto sleep 대상에서 제외

## 실행 흐름

### `ax up`

1. daemon 시작
2. workspace artifact 준비
3. orchestrator artifact 준비
4. 세션은 대부분 실제 작업 dispatch 시 시작

### 작업 dispatch

1. orchestrator 또는 workspace가 메시지 / task 생성
2. daemon이 queue/task state에 기록
3. session manager가 대상 session을 ensure runnable
4. 필요하면 session 생성 또는 재시작
5. tmux wake prompt 전송

### idle sleep

daemon session manager가 주기적으로 확인합니다.

workspace가 아래 조건을 모두 만족하면 sleep 후보가 됩니다.

- orchestrator가 아님
- idle timeout 경과
- tmux session이 실제로 idle
- queued message 없음
- wake retry pending 없음
- open assigned task 없음

## root vs child orchestrator

| 구분 | root orchestrator | child orchestrator |
|---|---|---|
| 실행 방식 | `ax claude` / `ax codex` | managed tmux session |
| 기본 성격 | foreground / ephemeral | background / managed |
| hot reload | 수동 relaunch 필요 | reconcile / restart 가능 |
| auto sleep | 해당 없음 | 제외(always-on policy) |

## runtime context와 durable state

runtime native memory는 runtime마다 다르게 동작합니다.

- Claude: `--continue` 기반 session reuse 성향
- Codex: stable `CODEX_HOME` 재사용 중심

`ax`는 이 차이를 줄이기 위해 daemon durable state를 별도로 가집니다.

- task state: runtime 재시작과 별도로 유지
- durable memory: project/workspace/global 수준 기억 유지

즉, session이 새로 떠도 daemon state는 남아 있고, orchestrator/worker는 MCP 도구로 이를 다시 읽어 복구할 수 있습니다.
