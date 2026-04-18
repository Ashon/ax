# 개발 가이드

이 문서는 `ax` 개발에 필요한 최소 흐름을 정리합니다. 더 자세한 구현 레퍼런스는 [DEVELOPER_GUIDE.md](../DEVELOPER_GUIDE.md)를 참고하세요.

## 저장소 구조

```text
crates/
  ax-cli/          CLI 엔트리(바이너리 `ax`)
  ax-agent/        runtime 통합(claude / codex)
  ax-config/       config 로딩 / project tree
  ax-daemon/       daemon, queue, task, session manager
  ax-mcp-server/   MCP server와 daemon client
  ax-proto/        wire 타입(Envelope, Payload)
  ax-tmux/         tmux 세션 제어
  ax-tui/          ratatui 기반 watch TUI
  ax-usage/        transcript 파서 + usage 집계
  ax-workspace/    workspace/orchestrator artifact 생성과 reconcile
e2e/               live orchestration harness
docs/              사용자/운영/개발 문서
```

## 자주 쓰는 명령

```bash
make build
make test
cargo test --workspace
cargo test -p ax-daemon
cargo test -p ax-mcp-server
cargo test -p ax-workspace
```

## 문서 읽는 순서

- 사용자/운영 관점: [README](../README.md) → [docs/](README.md)
- 심화 구현 레퍼런스: [DEVELOPER_GUIDE.md](../DEVELOPER_GUIDE.md)
- 크레이트 ownership / local rules: `crates/*/AGENTS.md`
- 설계 노트: [docs/design/](design/)

## 구현 시 참고할 것

- root orchestrator는 foreground/ephemeral
- child orchestrator와 workspace는 on-demand managed session
- daemon이 task, queue, wake, session lifecycle, durable memory를 소유
- prompt/instruction artifact 변경은 runtime behavior에 직접 영향

즉, 문서나 코드 수정 시에도 "artifact 생성 시점"과 "runtime/session 재시작 필요 여부"를 함께 봐야 합니다.
