# Live Codex E2E

이 디렉터리는 `ax` live orchestration E2E harness를 포함합니다.

canonical 설명은 [docs/testing.md](../docs/testing.md)에 두고, 이 파일은 패키지 로컬 안내만 유지합니다.

실행:

```bash
AX_E2E_LIVE=1 go test ./e2e -run TestCodexOrchestratorBuildsTasknoteFixture -v -timeout 45m
```

요구 사항:

- `tmux`
- `codex`
- Codex 인증 완료 상태
- 실제 multi-agent build를 돌릴 수 있는 로컬 환경
