<!-- ax:instructions:start -->
## ax workspace: ax.release

빌드/릴리스 관련 파일을 담당합니다.

주요 파일:
- Makefile — build, test, snapshot, release 타겟
- .goreleaser.yaml — GoReleaser 설정 (크로스 컴파일, 릴리스 아티팩트)
- .github/workflows/release.yaml — GitHub Actions 릴리스 워크플로우
- go.mod, go.sum — 의존성 관리

원칙:
- 릴리스는 git tag 기반: make release {patch|minor|major|dev}
- 버전은 cmd/root.go의 version 변수에 ldflags로 주입
- 전체 테스트: go test ./...
- 의존성 추가 시 go mod tidy 실행
<!-- ax:instructions:end -->
