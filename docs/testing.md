# 테스트와 Live E2E

`ax`는 일반 Cargo 테스트와 opt-in live Codex E2E를 함께 사용합니다.

## 일반 테스트

전체:

```bash
cargo test --workspace
```

크레이트 단위 예시:

```bash
cargo test -p ax-config
cargo test -p ax-daemon
cargo test -p ax-mcp-server
cargo test -p ax-workspace
cargo test -p ax-cli
```

특정 테스트만:

```bash
cargo test -p ax-daemon -- task_store::tests::refresh
```

## Live Codex E2E

이 프로젝트에는 실제 `codex` CLI, `tmux`, isolated sandbox를 사용하는 live orchestration 및 init 시나리오가 있습니다.

테스트 위치:

- harness: [e2e/src/harness.rs](../e2e/src/harness.rs)
- orchestration 시나리오: [e2e/tests/orchestration_live.rs](../e2e/tests/orchestration_live.rs)
- init 시나리오: [e2e/tests/init_live.rs](../e2e/tests/init_live.rs)
- fixture: [e2e/scenarios/](../e2e/scenarios)

### 검증하는 것

1. 현재 checkout의 `ax` 바이너리 빌드
2. 시나리오 fixture를 임시 sandbox로 복사
3. isolated `HOME`, `XDG_STATE_HOME`, `TMUX_TMPDIR`, daemon socket 사용
4. `ax up`
5. root `codex` orchestrator tmux session 실행
6. 단일 prompt로 delegated build 수행
7. fixture 결과물에 대해 `cargo test` + `cargo build` 검증 스크립트 실행

즉, "실제 runtime과 tmux를 써서 multi-agent orchestration이 끝까지 수렴하는지"를 검증합니다.

### 실행

```bash
AX_E2E_LIVE=1 cargo test -p ax-e2e --test orchestration_live -- --nocapture
AX_E2E_LIVE=1 cargo test -p ax-e2e --test init_live -- --nocapture
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

크레이트 로컬 설명은 [e2e/README.md](../e2e/README.md)에 있고, canonical testing overview는 이 문서에 둡니다.
