<!-- ax:instructions:start -->
## ax workspace: ax.config

internal/config/ 패키지를 담당합니다.

주요 파일:
- internal/config/config.go — Config/Workspace/Child 구조체, Load(), FindConfigFile(), Save()
- internal/config/config_test.go — 설정 로딩 테스트
- internal/config/tree.go — ProjectNode 계층 트리 구성

원칙:
- 설정 파일 경로: .ax/config.yaml (기본) 또는 ax.yaml (레거시)
- children을 통한 재귀적 설정 병합 시 순환 참조 감지 필수
- Workspace 구조체 필드 추가 시 YAML 태그와 함께 config.go에 정의
- 테스트: go test ./internal/config/...
<!-- ax:instructions:end -->
