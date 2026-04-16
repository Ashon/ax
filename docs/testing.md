# 테스트와 Live E2E

`ax`는 일반 Go 테스트와 opt-in live Codex E2E를 함께 사용합니다.

## 일반 테스트

전체:

```bash
go test ./...
```

패키지 단위 예시:

```bash
go test ./internal/config
go test ./internal/daemon
go test ./internal/mcpserver
go test ./internal/workspace
go test ./cmd/...
```

sandbox 환경에서는 기본 Go build cache 경로가 막힐 수 있으므로 필요하면 `GOCACHE`를 별도로 지정하세요.

```bash
env GOCACHE=/tmp/ax-gocache go test ./internal/workspace
```

## Live Codex E2E

이 프로젝트에는 실제 `codex` CLI, `tmux`, isolated sandbox를 사용하는 live orchestration test가 있습니다.

테스트 위치:

- harness: [e2e/orchestration_codex_live_test.go](../e2e/orchestration_codex_live_test.go)
- fixture: [e2e/testdata/tasknote](../e2e/testdata/tasknote)

### 검증하는 것

1. 현재 checkout의 `ax` binary 빌드
2. toy fixture를 임시 sandbox로 복사
3. isolated `HOME`, `XDG_STATE_HOME`, `TMUX_TMPDIR`, daemon socket 사용
4. `ax up`
5. root `codex` orchestrator tmux session 실행
6. 단일 prompt로 delegated build 수행
7. fixture 결과물에 대해 `go test ./...` + `go build ./cmd/tasknote`

즉, “실제 runtime과 tmux를 써서 multi-agent orchestration이 끝까지 수렴하는지”를 검증합니다.

### 실행

```bash
AX_E2E_LIVE=1 go test ./e2e -run TestCodexOrchestratorBuildsTasknoteFixture -v -timeout 45m
```

### 요구 사항

- `tmux`
- `codex`
- Codex 인증 완료 상태
- runtime이 실제로 작업할 수 있는 시간/로컬 자원

### 격리 방식

현재 harness는 다음을 분리합니다.

- sandbox `HOME`
- sandbox `XDG_STATE_HOME`
- sandbox `TMUX_TMPDIR`
- sandbox daemon socket
- sandboxed Codex config / ax MCP
- host tmux leak cleanup

즉, 기본적으로 live E2E가 host tmux와 host Codex config를 오염시키지 않도록 설계되어 있습니다.

## 문서 위치

패키지 로컬 설명은 [e2e/README.md](../e2e/README.md)에 있고, canonical testing overview는 이 문서에 둡니다.
