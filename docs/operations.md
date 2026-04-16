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
