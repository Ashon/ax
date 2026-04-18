# ax

tmux 기반 멀티 에이전트 LLM 워크스페이스 매니저

`ax`는 Claude, Codex 같은 코딩 에이전트를 tmux 세션으로 분리해 실행하고, daemon + MCP(Model Context Protocol)로 에이전트 간 통신, task 추적, on-demand session lifecycle, durable memory를 제공하는 도구입니다.

```
사용자
  └─ ax claude / ax codex
       └─ root orchestrator
            ├─ workspace agents
            ├─ child orchestrators
            └─ ax daemon (message/task/memory broker)
```

## 핵심 특징

- tmux 세션 기반 워크스페이스 격리
- daemon + MCP 기반 에이전트 간 메시지/도구 통신
- 계층적 프로젝트 트리와 서브 오케스트레이터
- on-demand workspace start / wake / idle sleep
- task 추적, stale 신호, recovery 액션
- durable memory (`remember_memory`, `recall_memories`, `list_memories`, `supersede_memory`)
- opt-in live Codex E2E 하네스

## 빠른 시작

```bash
cd /path/to/project
ax init
ax up
ax claude   # 또는 ax codex
```

- `ax up`은 daemon을 시작하고 workspace / orchestrator artifact를 준비합니다.
- 루트 오케스트레이터는 `ax claude` 또는 `ax codex`로 포그라운드 실행합니다.
- workspace와 child orchestrator는 작업이 dispatch될 때 on-demand로 시작됩니다.

작업 상태와 세션은 다음 명령으로 확인할 수 있습니다.

```bash
ax status
ax top
ax tasks
ax workspace list
```

정리:

```bash
ax down
```

## 설치

### 요구 사항

- `tmux` 3.x 이상
- `claude` CLI (Claude runtime 사용 시)
- `codex` CLI (Codex runtime 사용 시)

### 설치 방법

```bash
# release binary (macOS arm64 예시)
curl -Lo ax.tar.gz https://github.com/Ashon/ax/releases/latest/download/ax-aarch64-darwin.tar.gz
tar xzf ax.tar.gz
sudo mv ax /usr/local/bin/
```

```bash
# from source (Rust 1.88+ 필요)
git clone https://github.com/Ashon/ax.git
cd ax
make install
```

## 저장소 구조

```
ax/
├── crates/                 Cargo workspace members
│   ├── ax-cli              binary entry (ax)
│   ├── ax-tui              ratatui watch TUI
│   ├── ax-daemon           Unix socket daemon
│   ├── ax-mcp-server       MCP stdio server (33 tools)
│   ├── ax-workspace        lifecycle + reconcile + artifacts
│   ├── ax-config           .ax/config.yaml schema + tree
│   ├── ax-agent            runtime + launch helpers
│   ├── ax-tmux             tmux session wrappers
│   ├── ax-proto            wire types
│   └── ax-usage            token + trend aggregators
├── e2e/                    cross-crate smoke + live tests
├── docs/                   사용자/개발자 문서
└── Makefile                cargo wrapper (build / install / test)
```

## 문서

- [문서 인덱스](docs/README.md)
- [빠른 시작](docs/getting-started.md)
- [설정 가이드](docs/configuration.md)
- [아키텍처](docs/architecture.md)
- [운영 가이드](docs/operations.md)
- [Tasks / Durable Memory](docs/tasks-and-memory.md)
- [테스트와 Live E2E](docs/testing.md)
- [개발 가이드](docs/development.md)

심화 구현 레퍼런스는 [DEVELOPER_GUIDE.md](DEVELOPER_GUIDE.md), 설계 노트는 [docs/design/workspace-usage.md](docs/design/workspace-usage.md)에 남겨 두었습니다.
