<!-- ax:instructions:start -->
## ax workspace: ax.runtime

internal/agent/, internal/workspace/, internal/tmux/ 패키지를 담당합니다.

주요 파일:
- internal/agent/runtime.go — Runtime 인터페이스 정의 및 Get() 팩토리
- internal/agent/claude.go, codex.go, shell.go — 런타임 구현체
- internal/workspace/workspace.go — Manager: Create/Destroy/CreateAll/DestroyAll
- internal/workspace/orchestrator.go — 오케스트레이터 프롬프트 생성
- internal/workspace/instructions.go — 에이전트 지시 파일(CLAUDE.md 등) 생성
- internal/workspace/mcpconfig.go — .mcp.json 생성
- internal/tmux/tmux.go — tmux 세션 생성/파괴/어태치/키전송

원칙:
- 새 런타임 추가 시 Runtime 인터페이스를 구현하고 runtime.go의 Get()에 등록
- tmux 세션 이름은 SessionPrefix("ax-") + 워크스페이스 이름 규칙을 따름
- 테스트: go test ./internal/agent/... ./internal/workspace/...
<!-- ax:instructions:end -->
