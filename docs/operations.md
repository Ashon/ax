# 운영 가이드

이 문서는 `ax`를 실제로 운용할 때 자주 쓰는 명령과 session lifecycle을 정리합니다.

## 핵심 명령

```bash
ax up
ax down
ax status
ax top
ax refresh
ax workspace list
ax workspace attach <name>
ax workspace interrupt <name>
ax tasks
ax messages
```

## lifecycle 요약

### `ax up`

- daemon 시작
- workspace / orchestrator artifact 준비
- root orchestrator는 시작하지 않음
- workspace / child orchestrator는 on-demand로 시작

### root orchestrator

```bash
ax claude
ax codex
```

특성:

- foreground 세션
- 사용자와 직접 대화
- 종료하면 root session도 종료

### workspace / child orchestrator

특성:

- dispatch 시 session이 없으면 생성
- pending work가 있으면 wake scheduler가 다시 깨움
- workspace는 idle 상태가 오래 지속되면 sleep 가능
- orchestrator 계열은 auto sleep 대상에서 제외

## 관측

### 전체 상태

```bash
ax status
```

확인 항목:

- configured vs running session
- online / offline / detached 상태
- root / child orchestrator 구조

### 실시간 모니터링

```bash
ax top
```

확인 항목:

- 활동 중인 session
- usage / token trend
- 최근 pane 상태

`ax top`의 색상은 보조 신호입니다. 중요한 상태는 텍스트, `●`/`○`
마커, 선택 row의 reverse-video, bold/dim modifier로도 남기 때문에
저색상 터미널이나 흑백 capture에서도 읽을 수 있어야 합니다.
foreground 색을 끄고 확인하려면 표준 `NO_COLOR=1 ax top` 또는
ax 전용 `AX_TUI_NO_COLOR=1 ax top`을 사용합니다. 이 경우 색만 빠지고
bold, dim, reverse-video 선택 표시는 유지됩니다.

기본 색 의미는 다음처럼 고정됩니다. cyan은 focus/running/progress, green은
online/done/clean/success, yellow는 blocked/stale/dirty/mixed/warning, red는
failed/error, gray는 timestamp/placeholder/metadata, light blue/magenta/yellow/cyan은
workspace/sender, task id/down token, cost, info 값 보조 표시입니다.

agents view의 주요 컬럼:

- `NAME`: 선택 cursor, live/offline 마커, project/workspace label
- `STATE`: `running`, `idle`, `online`, `offline`, `disconnected`
- `UP` / `DOWN` / `COST`: 현재 capture 또는 누적 trend에서 읽은 token/cost
- `INFO`: status/reconcile note와 group-level git 요약

git 요약은 agent leaf row마다 반복하지 않고 project/group row의 `INFO`에만
roll-up됩니다. 직접 child workspace들이 같은 상태이면 `git clean` 또는
`git changed:N ?M`처럼 표시하고, child 상태가 섞이면 `git mixed`로 표시합니다.
폭이 좁으면 `git ~N ?M`처럼 축약될 수 있습니다. 선택한 workspace의 상세
pane에는 modified/added/deleted/untracked와 diff 통계가 있는 full git detail이
나옵니다.

tasks view는 `ID`, `STATE`, `OWNER`, `TITLE` 컬럼을 쓰고, active stale task는
`running stale` / `pending stale`처럼 state 텍스트에 stale을 붙입니다. summary
line의 `msg`는 queued message, `div`는 task/message divergence, `hi`는 high 또는
urgent priority를 뜻합니다. messages view는 time, sender, recipient, optional
task id, body를 분리해서 보여주며 error/failure, blocked/stale/wake,
completed/done 같은 body 단어를 상태 색으로 보조 표시합니다.

### 특정 session 직접 확인

```bash
ax workspace attach <name>
```

### 멈춘 agent 인터럽트

```bash
ax workspace interrupt <name>
```

## 메시지 / task 운영

### 메시지

```bash
ax send <workspace> "<message>"
ax messages
ax messages --wait
ax messages --json
```

### task

```bash
ax tasks
ax tasks show <task-id>
ax tasks activity <task-id>
```

task 개념과 durable memory는 [Tasks / Durable Memory](tasks-and-memory.md) 문서를 참고하세요.

## 설정 반영

```bash
ax refresh
ax refresh --start-missing
ax refresh --restart
```

언제 쓰는가:

- instruction / prompt / MCP config를 다시 만들고 싶을 때
- 꺼진 managed session을 같이 올리고 싶을 때
- runtime config를 바로 반영하기 위해 managed session을 재시작할 때

주의:

- root orchestrator는 foreground/ephemeral이라 `refresh --restart` 대상이 아닙니다.
- root prompt가 바뀌었으면 다시 `ax claude` / `ax codex`를 실행해야 합니다.

## daemon 직접 제어

필요하면 daemon subcommand를 직접 쓸 수 있습니다.

```bash
ax daemon start
ax daemon status
```

일반적으로는 `ax up` / `ax down`을 쓰는 편이 낫습니다.

## 문제 상황에서 먼저 볼 것

1. `ax status`로 configured/running mismatch 확인
2. `ax tasks`로 stale/open task 확인
3. `ax messages`로 inbox 정체 확인
4. `ax workspace attach <name>`로 실제 session 상태 확인
5. 필요한 경우 `ax refresh` 또는 `ax workspace interrupt <name>` 적용
