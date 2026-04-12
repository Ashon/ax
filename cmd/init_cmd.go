package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/ashon/ax/internal/config"
	"github.com/spf13/cobra"
	"gopkg.in/yaml.v3"
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

		alreadyExists := false
		if _, err := os.Stat(path); err == nil {
			alreadyExists = true
			fmt.Printf("%s already exists — skipping creation\n", path)
		} else if !initGlobal {
			if legacyPath, ok := configPathConflict(dir); ok {
				return fmt.Errorf("legacy config already exists at %s", legacyPath)
			}
		}

		if !alreadyExists {
			cfg := config.DefaultConfig(projectName)
			if err := cfg.Save(path); err != nil {
				return err
			}
			fmt.Printf("Created %s\n", path)
		}

		// Ensure this dir is registered as a child of any ancestor config.
		if !initGlobal {
			if parentPath, added := registerAsChild(dir, projectName); added {
				fmt.Printf("Registered as child of %s\n", parentPath)
			}
		}

		// Ensure .mcp.json is gitignored (it has user-specific paths).
		if !initGlobal {
			if added, _ := ensureGitignore(dir, ".mcp.json"); added {
				fmt.Println("Added .mcp.json to .gitignore")
			}
		}

		if alreadyExists || initNoSetup {
			if !alreadyExists {
				fmt.Println("Edit it to define your workspaces, then run: ax up")
			}
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
	userPrompt := "프로젝트 구조를 파악해서 워크스페이스 구성을 결정하고 config.yaml에 작성해주세요. 작성 완료 후 어떤 워크스페이스를 만들었는지 요약해주세요."

	claudeBin, err := exec.LookPath("claude")
	if err != nil {
		fmt.Println("claude CLI not found — skipping setup.")
		fmt.Printf("Edit %s manually and run: ax up\n", configPath)
		return nil
	}

	cmd := exec.Command(claudeBin,
		"-p",
		"--dangerously-skip-permissions",
		"--output-format", "stream-json",
		"--verbose",
		"--append-system-prompt", systemPrompt,
		userPrompt,
	)
	cmd.Dir = projectDir
	cmd.Stdin = nil
	cmd.Stderr = os.Stderr

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("stdout pipe: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("start claude: %w", err)
	}

	finalText, uiErr := streamClaudeOutput(stdout)
	waitErr := cmd.Wait()

	if finalText != "" {
		fmt.Println(finalText)
	}
	if uiErr != nil {
		return uiErr
	}
	return waitErr
}

// streamClaudeOutput runs a bubbletea spinner UI while parsing
// claude's stream-json output in a background goroutine. Returns
// the final assistant text so the caller can print it after the UI exits.
func streamClaudeOutput(r io.Reader) (string, error) {
	m := newSetupModel()
	p := tea.NewProgram(m)

	go func() {
		parseClaudeStream(r, p)
		p.Send(setupDoneMsg{})
	}()

	finalModel, err := p.Run()
	if err != nil {
		return "", err
	}
	if sm, ok := finalModel.(setupModel); ok {
		return sm.finalText, nil
	}
	return "", nil
}

type setupStatusMsg string
type setupTextMsg string
type setupDoneMsg struct{}
type setupTickMsg time.Time

var (
	setupSpinnerStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("6"))
	setupStatusStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("7"))
)

type setupModel struct {
	status    string
	finalText string
	frame     int
	done      bool
}

func newSetupModel() setupModel {
	return setupModel{status: "Analyzing project..."}
}

func (m setupModel) Init() tea.Cmd {
	return setupTickCmd()
}

func setupTickCmd() tea.Cmd {
	return tea.Tick(80*time.Millisecond, func(t time.Time) tea.Msg {
		return setupTickMsg(t)
	})
}

func (m setupModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case setupTickMsg:
		m.frame++
		if m.done {
			return m, nil
		}
		return m, setupTickCmd()
	case setupStatusMsg:
		m.status = string(msg)
	case setupTextMsg:
		m.finalText = string(msg)
	case setupDoneMsg:
		m.done = true
		return m, tea.Quit
	}
	return m, nil
}

func (m setupModel) View() string {
	if m.done {
		return ""
	}
	frames := []string{"⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"}
	spinner := setupSpinnerStyle.Render(frames[m.frame%len(frames)])
	return spinner + " " + setupStatusStyle.Render(m.status)
}

func parseClaudeStream(r io.Reader, p *tea.Program) {
	scanner := bufio.NewScanner(r)
	scanner.Buffer(make([]byte, 1024*1024), 16*1024*1024)
	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}
		var evt map[string]any
		if err := json.Unmarshal(line, &evt); err != nil {
			continue
		}

		msgType, _ := evt["type"].(string)
		if msgType != "assistant" {
			continue
		}
		msg, ok := evt["message"].(map[string]any)
		if !ok {
			continue
		}
		content, ok := msg["content"].([]any)
		if !ok {
			continue
		}

		for _, item := range content {
			block, ok := item.(map[string]any)
			if !ok {
				continue
			}
			switch block["type"] {
			case "text":
				if text, ok := block["text"].(string); ok && text != "" {
					p.Send(setupTextMsg(text))
					first := strings.SplitN(strings.TrimSpace(text), "\n", 2)[0]
					if len(first) > 60 {
						first = first[:57] + "..."
					}
					p.Send(setupStatusMsg("Thinking: " + first))
				}
			case "tool_use":
				name, _ := block["name"].(string)
				p.Send(setupStatusMsg(describeToolUse(name, block)))
			}
		}
	}
}

func describeToolUse(name string, block map[string]any) string {
	input, _ := block["input"].(map[string]any)
	switch name {
	case "Read":
		if p, ok := input["file_path"].(string); ok {
			return "Reading " + shortPath(p)
		}
	case "Write":
		if p, ok := input["file_path"].(string); ok {
			return "Writing " + shortPath(p)
		}
	case "Edit":
		if p, ok := input["file_path"].(string); ok {
			return "Editing " + shortPath(p)
		}
	case "Glob":
		if pat, ok := input["pattern"].(string); ok {
			return "Searching " + pat
		}
	case "Grep":
		if pat, ok := input["pattern"].(string); ok {
			return "Grepping " + pat
		}
	case "Bash":
		if d, ok := input["description"].(string); ok && d != "" {
			return "Running: " + d
		}
	}
	return "Using " + name
}

func shortPath(p string) string {
	if home, err := os.UserHomeDir(); err == nil && strings.HasPrefix(p, home) {
		p = "~" + strings.TrimPrefix(p, home)
	}
	parts := strings.Split(p, "/")
	if len(parts) > 3 {
		return ".../" + strings.Join(parts[len(parts)-3:], "/")
	}
	return p
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

// registerAsChild searches upward from dir for an ancestor .ax/config.yaml
// and registers dir as a child entry if not already present.
// Returns the parent config path and whether a new entry was added.
func registerAsChild(dir, name string) (string, bool) {
	parent := filepath.Dir(dir)
	for {
		if parent == dir || parent == "" {
			break
		}
		if path, ok := findConfigInDir(parent); ok {
			added, _ := addChildToConfig(path, parent, dir, name)
			if added {
				return path, true
			}
			return "", false
		}
		next := filepath.Dir(parent)
		if next == parent {
			break
		}
		dir = parent
		parent = next
	}
	return "", false
}

func findConfigInDir(dir string) (string, bool) {
	preferred := config.DefaultConfigPath(dir)
	if _, err := os.Stat(preferred); err == nil {
		return preferred, true
	}
	legacy := config.LegacyConfigPath(dir)
	if _, err := os.Stat(legacy); err == nil {
		return legacy, true
	}
	return "", false
}

// addChildToConfig adds childDir to the parent config's children map.
// Returns (added, err): added=true if a new entry was written.
func addChildToConfig(parentConfigPath, parentDir, childDir, childName string) (bool, error) {
	parentCfg, err := loadRawConfig(parentConfigPath)
	if err != nil {
		return false, err
	}
	if parentCfg.Children == nil {
		parentCfg.Children = make(map[string]config.Child)
	}

	relDir, err := filepath.Rel(parentDir, childDir)
	if err != nil {
		relDir = childDir
	}

	// Check if any existing entry already points to this directory
	for _, existing := range parentCfg.Children {
		if existing.Dir == relDir {
			return false, nil
		}
	}

	// Pick a unique entry name
	entryName := childName
	for i := 2; ; i++ {
		if _, exists := parentCfg.Children[entryName]; !exists {
			break
		}
		entryName = fmt.Sprintf("%s-%d", childName, i)
	}

	parentCfg.Children[entryName] = config.Child{Dir: relDir}
	if err := parentCfg.Save(parentConfigPath); err != nil {
		return false, err
	}
	return true, nil
}

// loadRawConfig reads and parses a config file without resolving children,
// so we can edit and re-save it without merging the tree.
func loadRawConfig(path string) (*config.Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var cfg config.Config
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, err
	}
	return &cfg, nil
}

// ensureGitignore appends pattern to .gitignore in dir if it's not already
// present. Returns (added, err): added=true if the file was modified.
// If .gitignore does not exist and the directory is not a git repository,
// no file is created.
func ensureGitignore(dir, pattern string) (bool, error) {
	gitignorePath := filepath.Join(dir, ".gitignore")

	data, err := os.ReadFile(gitignorePath)
	if err != nil && !os.IsNotExist(err) {
		return false, err
	}
	if os.IsNotExist(err) {
		// Only create if this directory has a .git folder
		if _, gitErr := os.Stat(filepath.Join(dir, ".git")); gitErr != nil {
			return false, nil
		}
		data = nil
	}

	// Check if pattern already present (exact line match)
	for _, line := range strings.Split(string(data), "\n") {
		if strings.TrimSpace(line) == pattern {
			return false, nil
		}
	}

	content := string(data)
	if len(content) > 0 && !strings.HasSuffix(content, "\n") {
		content += "\n"
	}
	content += pattern + "\n"

	if err := os.WriteFile(gitignorePath, []byte(content), 0o644); err != nil {
		return false, err
	}
	return true, nil
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
