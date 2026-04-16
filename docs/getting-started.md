# 빠른 시작

이 문서는 `ax`를 처음 켜서 루트 오케스트레이터와 워크스페이스를 실제로 움직이는 최소 흐름을 설명합니다.

## 1. 요구 사항

- `tmux` 3.x 이상
- `claude` CLI 또는 `codex` CLI
- `ax` 바이너리

설치 방법은 [README](../README.md)를 참고하세요.

## 2. 프로젝트 초기화

프로젝트 루트에서:

```bash
ax init
```

Codex 중심으로 시작하고 싶다면:

```bash
ax init --codex
```

생성 결과:

- `.ax/config.yaml`
- workspace별 runtime/instructions 기본값
- 필요 시 자식 프로젝트 연결 준비

## 3. daemon 및 artifact 준비

```bash
ax up
```

현재 구현 기준으로 `ax up`은 다음을 합니다.

- daemon 시작
- workspace `.mcp.json` 및 instruction file 준비
- orchestrator prompt / MCP config 준비
- root orchestrator는 자동 실행하지 않음
- workspace / child orchestrator는 on-demand dispatch 시 시작

즉, `ax up`은 “모든 세션을 미리 띄우는 명령”이 아니라 “실행 가능한 상태를 준비하는 명령”입니다.

## 4. 루트 오케스트레이터 실행

```bash
ax claude
```

또는:

```bash
ax codex
```

루트 오케스트레이터 특성:

- foreground tmux session으로 실행
- 사용자 요청을 받아 workspace 또는 child orchestrator에 분배
- 종료하면 root session도 함께 종료
- child orchestrator / workspace 세션은 필요에 따라 별도로 살아 있을 수 있음

## 5. 상태 확인

자주 쓰는 명령:

```bash
ax status
ax top
ax workspace list
ax tasks
ax messages
```

권장 확인 순서:

- `ax status`: configured session과 현재 연결 상태 확인
- `ax top`: 활동, usage, tmux 상태 모니터링
- `ax tasks`: task 진행/정체 여부 확인
- `ax workspace attach <name>`: 특정 세션 직접 확인

## 6. 종료

```bash
ax down
```

`ax down`은:

- workspace session 정리
- 살아 있는 orchestrator session 정리
- daemon 종료

## 7. 첫 운영 팁

- 루트 오케스트레이터는 “프로젝트 리더”, workspace는 “실제 작업자”로 생각하면 됩니다.
- 루트는 직접 코드를 수정하기보다 적절한 owner에게 task를 분배하도록 설계되어 있습니다.
- fresh context가 필요한 작업은 task start mode를 `fresh`로 쓰는 흐름이 이미 준비되어 있습니다.
- 재시작 이후에도 남아야 할 결정사항은 durable memory로 남기는 편이 좋습니다. 자세한 내용은 [Tasks / Durable Memory](tasks-and-memory.md)를 참고하세요.
