package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"

	"github.com/ashon/ax/internal/config"
	"github.com/spf13/cobra"
)

var (
	initGlobal  bool
	initNoSetup bool
)

var initCmd = &cobra.Command{
	Use:   "init",
	Short: "Initialize .ax/config.yaml (interactively via a setup agent by default)",
	RunE: func(cmd *cobra.Command, args []string) error {
		var dir, path, projectName string

		if initGlobal {
			home, err := os.UserHomeDir()
			if err != nil {
				return fmt.Errorf("resolve home directory: %w", err)
			}
			dir = home
			path = config.DefaultConfigPath(dir)
			projectName = "global"
		} else {
			dir = mustGetwd()
			path = config.DefaultConfigPath(dir)
			projectName = filepath.Base(dir)
		}

		if _, err := os.Stat(path); err == nil {
			return fmt.Errorf("%s already exists", path)
		}
		if !initGlobal {
			if legacyPath, ok := configPathConflict(dir); ok {
				return fmt.Errorf("legacy config already exists at %s", legacyPath)
			}
		}

		cfg := config.DefaultConfig(projectName)
		if err := cfg.Save(path); err != nil {
			return err
		}
		fmt.Printf("Created %s\n", path)

		if initNoSetup {
			fmt.Println("Edit it to define your workspaces, then run: ax up")
			return nil
		}

		// Launch setup agent to analyze the project and flesh out the config
		fmt.Println("\nLaunching setup agent (claude)...")
		fmt.Println("The agent will analyze your project and help define workspaces.")
		fmt.Println()
		return runSetupAgent(dir, path)
	},
}

func runSetupAgent(projectDir, configPath string) error {
	systemPrompt := buildSetupSystemPrompt(configPath)
	userPrompt := "프로젝트 구조를 파악해서 워크스페이스 구성을 결정하고 바로 config.yaml에 작성해주세요. 작성 후 사용자에게 결과를 보여주고 조정이 필요하면 말씀해달라고 안내하세요."

	claudeBin, err := exec.LookPath("claude")
	if err != nil {
		fmt.Println("claude CLI not found — skipping interactive setup.")
		fmt.Printf("Edit %s manually and run: ax up\n", configPath)
		return nil
	}

	cmd := exec.Command(claudeBin,
		"--dangerously-skip-permissions",
		"--append-system-prompt", systemPrompt,
		userPrompt,
	)
	cmd.Dir = projectDir
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

func buildSetupSystemPrompt(configPath string) string {
	return fmt.Sprintf(`당신은 ax 프로젝트 셋업 에이전트입니다. 사용자가 요청하면 현재 디렉토리의 프로젝트를 분석해서 멀티 에이전트 워크스페이스 구성을 제안하고 %s 파일을 편집하세요.

## 절차
1. 프로젝트 구조를 파악하세요 (Glob으로 디렉토리 구조, README/package.json/go.mod/pyproject.toml 등 주요 파일 확인).
2. 모노레포인지, 어떤 도메인들이 있는지, 어떤 역할의 에이전트가 필요한지 판단하세요.
3. **사용자에게 확인을 묻지 말고** 바로 %s 파일을 편집하세요.
4. 편집 후 최종 구성을 요약해서 보여주고, 사용자가 조정을 요청하면 반영하세요.

## config.yaml 형식
` + "```yaml" + `
project: <프로젝트 이름>
workspaces:
  <name>:
    dir: <프로젝트 루트 기준 상대 경로>
    description: <해당 에이전트의 역할 한 문장>
    runtime: claude  # 또는 codex
    instructions: |
      <해당 워크스페이스 에이전트가 받을 지침 — 무엇을 해야 하는지, 어떤 파일을 건드려야 하는지 등>
` + "```" + `

## 주의사항
- 워크스페이스 이름은 kebab-case 또는 snake_case로 짧고 명확하게 (예: backend, frontend, infra, docs).
- description은 한 문장으로 역할을 명확히 설명.
- instructions는 구체적으로 작성 — 그 에이전트가 어떤 디렉토리에서 작업하고, 어떤 원칙을 따라야 하는지.
- 기존 %s 파일은 최소 stub만 있는 상태입니다. workspaces 섹션을 채워주세요.`, configPath, configPath, configPath)
}

func mustGetwd() string {
	dir, err := os.Getwd()
	if err != nil {
		return "."
	}
	return dir
}

func configPathConflict(dir string) (string, bool) {
	if path, ok := configPathExists(config.DefaultConfigPath(dir)); ok {
		return path, true
	}
	if path, ok := configPathExists(config.LegacyConfigPath(dir)); ok {
		return path, true
	}
	return "", false
}

func configPathExists(path string) (string, bool) {
	if _, err := os.Stat(path); err == nil {
		return path, true
	}
	return "", false
}

func init() {
	initCmd.Flags().BoolVarP(&initGlobal, "global", "g", false, "initialize global config at ~/.ax/config.yaml")
	initCmd.Flags().BoolVar(&initNoSetup, "no-setup", false, "skip the interactive setup agent")
	rootCmd.AddCommand(initCmd)
}
