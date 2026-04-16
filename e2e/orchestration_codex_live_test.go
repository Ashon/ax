package e2e

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/types"
)

const (
	liveE2EEnv          = "AX_E2E_LIVE"
	liveE2ETimeout      = 25 * time.Minute
	liveE2EPollInterval = 10 * time.Second
	liveE2ESettleWindow = 15 * time.Second
	liveRootPrompt      = "Build the tasknote toy project end-to-end. Requirements: implement add/list/done/export-markdown commands backed by a local tasks.json file; task ids start at 1; list prints lines like `1. [ ] title`; markdown export prints lines like `- [ ] title`; delegate pure task logic and markdown rendering to the core workspace; delegate file I/O and command UX to the cli workspace; use start_task for delegated work; do not modify tests; finish only when `go test ./...` passes in both workspaces and `go build ./cmd/tasknote` passes in cli."
)

func TestCodexOrchestratorBuildsTasknoteFixture(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping live Codex orchestration e2e in short mode")
	}
	if os.Getenv(liveE2EEnv) != "1" {
		t.Skip("set AX_E2E_LIVE=1 to run the live Codex orchestration e2e")
	}

	requireTool(t, "go")
	requireTool(t, "tmux")
	requireTool(t, "codex")

	repoRoot := repoRoot(t)
	fixtureRoot := filepath.Join(repoRoot, "e2e", "testdata", "tasknote")
	hostHome, err := os.UserHomeDir()
	if err != nil {
		t.Fatalf("resolve host home: %v", err)
	}
	hostEnv := append([]string(nil), os.Environ()...)
	sandboxRoot, err := os.MkdirTemp("/tmp", "ax-e2e-")
	if err != nil {
		t.Fatalf("mkdir temp sandbox: %v", err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(sandboxRoot) })

	projectRoot := filepath.Join(sandboxRoot, "p")
	copyTree(t, fixtureRoot, projectRoot)

	homeDir := filepath.Join(sandboxRoot, "h")
	stateDir := filepath.Join(sandboxRoot, "s")
	tmuxTmpDir := filepath.Join(sandboxRoot, "t")
	mustMkdirAll(t, homeDir)
	mustMkdirAll(t, stateDir)
	mustMkdirAll(t, tmuxTmpDir)
	seedCodexBaseHome(t, hostHome, homeDir)

	env := append(filteredEnv(os.Environ(), "TMUX", "TMUX_PANE"),
		"HOME="+homeDir,
		"XDG_STATE_HOME="+stateDir,
		"TMUX_TMPDIR="+tmuxTmpDir,
		"NO_COLOR=1",
	)
	t.Cleanup(func() {
		_ = cleanupHostLeakedTmuxSessions(context.Background(), hostEnv, sandboxRoot)
	})

	axBin := filepath.Join(sandboxRoot, "ax")
	if _, err := runCommand(context.Background(), repoRoot, env, "go", "build", "-o", axBin, "."); err != nil {
		t.Fatalf("build ax binary: %v", err)
	}

	configPath := filepath.Join(projectRoot, ".ax", "config.yaml")
	socketPath := filepath.Join(sandboxRoot, "d.sock")
	daemonCmd, daemonLogs, err := startDaemon(axBin, projectRoot, env, socketPath)
	if err != nil {
		t.Fatalf("start daemon: %v\n\ndaemon logs:\n%s", err, daemonLogs.String())
	}
	t.Cleanup(func() {
		stopProcess(daemonCmd)
	})
	if _, err := runCommand(context.Background(), projectRoot, env, axBin, "--config", configPath, "--socket", socketPath, "up"); err != nil {
		t.Fatalf("ax up: %v\n\ndaemon logs:\n%s", err, daemonLogs.String())
	}
	t.Cleanup(func() {
		_, _ = runCommand(context.Background(), projectRoot, env, axBin, "--config", configPath, "--socket", socketPath, "down")
	})

	orchestratorDir := filepath.Join(homeDir, ".ax", "orchestrator")
	sessionName := "ax-e2e-root-" + strconv.FormatInt(time.Now().UnixNano(), 10)
	if err := startRootSession(orchestratorDir, sessionName, env, axBin, socketPath, configPath); err != nil {
		t.Fatalf("start root orchestrator session: %v", err)
	}
	t.Cleanup(func() {
		_, _ = runCommand(context.Background(), projectRoot, env, "tmux", "kill-session", "-t", sessionName)
	})

	ctx, cancel := context.WithTimeout(context.Background(), liveE2ETimeout)
	defer cancel()

	if err := waitForSessionIdle(ctx, env, sessionName); err != nil {
		t.Fatalf("wait for root orchestrator prompt: %v\n\npane:\n%s", err, capturePane(env, sessionName))
	}
	if err := sendRootPrompt(ctx, env, sessionName, homeDir, liveRootPrompt); err != nil {
		t.Fatalf("send root prompt: %v", err)
	}

	if err := waitForSuccessfulBuild(ctx, env, sessionName, projectRoot, socketPath); err != nil {
		t.Fatalf("live orchestration e2e failed: %v\n\npane:\n%s\n\ntasks:\n%s", err, capturePane(env, sessionName), readFileBestEffort(daemon.TasksFilePath(socketPath)))
	}
}

func waitForSuccessfulBuild(ctx context.Context, env []string, sessionName, projectRoot, socketPath string) error {
	settledAt := time.Time{}
	for {
		select {
		case <-ctx.Done():
			return fmt.Errorf("timed out waiting for successful build")
		default:
		}

		validateErr := validateTasknote(projectRoot, env)
		tasks, tasksErr := readTasksSnapshot(daemon.TasksFilePath(socketPath))
		if tasksErr != nil {
			validateErr = joinErr(validateErr, tasksErr)
		}
		usedCore, usedCLI, openTasks := summarizeTasks(tasks)
		idle := paneLooksIdle(capturePane(env, sessionName))

		if validateErr == nil && usedCore && usedCLI && openTasks == 0 && idle {
			if settledAt.IsZero() {
				settledAt = time.Now()
			}
			if time.Since(settledAt) >= liveE2ESettleWindow {
				return nil
			}
		} else {
			settledAt = time.Time{}
		}

		time.Sleep(liveE2EPollInterval)
	}
}

func validateTasknote(projectRoot string, env []string) error {
	coreDir := filepath.Join(projectRoot, "core")
	cliDir := filepath.Join(projectRoot, "cli")

	if _, err := runCommand(context.Background(), coreDir, env, "go", "test", "./..."); err != nil {
		return fmt.Errorf("core validation failed: %w", err)
	}
	if _, err := runCommand(context.Background(), cliDir, env, "go", "test", "./..."); err != nil {
		return fmt.Errorf("cli tests failed: %w", err)
	}
	if _, err := runCommand(context.Background(), cliDir, env, "go", "build", "./cmd/tasknote"); err != nil {
		return fmt.Errorf("cli build failed: %w", err)
	}
	return nil
}

func summarizeTasks(tasks []types.Task) (usedCore bool, usedCLI bool, openTasks int) {
	for _, task := range tasks {
		switch task.Assignee {
		case "core":
			usedCore = true
		case "cli":
			usedCLI = true
		}
		switch task.Status {
		case types.TaskPending, types.TaskInProgress, types.TaskBlocked:
			openTasks++
		}
	}
	return usedCore, usedCLI, openTasks
}

func startRootSession(orchestratorDir, sessionName string, env []string, axBin, socketPath, configPath string) error {
	_, err := runCommand(context.Background(), orchestratorDir, env,
		"tmux", "new-session", "-d", "-s", sessionName, "-c", orchestratorDir,
		axBin, "run-agent",
		"--runtime", "codex",
		"--workspace", "orchestrator",
		"--socket", socketPath,
		"--config", configPath,
	)
	return err
}

func startDaemon(axBin, dir string, env []string, socketPath string) (*exec.Cmd, *bytes.Buffer, error) {
	cmd := exec.Command(axBin, "--socket", socketPath, "daemon", "start")
	cmd.Dir = dir
	cmd.Env = env
	var logs bytes.Buffer
	cmd.Stdout = &logs
	cmd.Stderr = &logs
	if err := cmd.Start(); err != nil {
		return nil, nil, err
	}
	if err := waitForSocket(socketPath, 5*time.Second); err != nil {
		stopProcess(cmd)
		return nil, &logs, fmt.Errorf("daemon socket did not appear: %w", err)
	}
	return cmd, &logs, nil
}

func waitForSocket(path string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(path); err == nil {
			return nil
		}
		time.Sleep(100 * time.Millisecond)
	}
	return fmt.Errorf("timed out waiting for %s", path)
}

func stopProcess(cmd *exec.Cmd) {
	if cmd == nil || cmd.Process == nil {
		return
	}
	_ = cmd.Process.Kill()
	_, _ = cmd.Process.Wait()
}

func waitForSessionIdle(ctx context.Context, env []string, sessionName string) error {
	for {
		select {
		case <-ctx.Done():
			return fmt.Errorf("timed out waiting for idle session")
		default:
		}
		if paneLooksIdle(capturePane(env, sessionName)) {
			return nil
		}
		time.Sleep(2 * time.Second)
	}
}

func sendTmuxText(env []string, sessionName, text string) error {
	if _, err := runCommand(context.Background(), "", env, "tmux", "send-keys", "-t", sessionName, "-l", text); err != nil {
		return err
	}
	time.Sleep(150 * time.Millisecond)
	_, err := runCommand(context.Background(), "", env, "tmux", "send-keys", "-t", sessionName, "Enter")
	return err
}

func sendRootPrompt(ctx context.Context, env []string, sessionName, homeDir, prompt string) error {
	sessionDir, err := waitForCodexSessionDir(ctx, homeDir, "orchestrator-*")
	if err != nil {
		return err
	}
	historyPath := filepath.Join(sessionDir, "history.jsonl")
	if err := sendTmuxText(env, sessionName, prompt); err != nil {
		return err
	}

	promptLead := "› " + promptPrefix(prompt, 32)
	lastEnter := time.Now()
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		if historyContains(historyPath, prompt) {
			return nil
		}

		pane := capturePane(env, sessionName)
		if strings.Contains(pane, promptLead) && time.Since(lastEnter) >= 3*time.Second {
			if _, err := runCommand(ctx, "", env, "tmux", "send-keys", "-t", sessionName, "Enter"); err != nil {
				return err
			}
			lastEnter = time.Now()
		}
		time.Sleep(1 * time.Second)
	}

	return fmt.Errorf("prompt was not accepted by Codex\n\npane:\n%s", capturePane(env, sessionName))
}

func capturePane(env []string, sessionName string) string {
	out, err := runCommand(context.Background(), "", env, "tmux", "capture-pane", "-t", sessionName, "-p")
	if err != nil {
		return err.Error()
	}
	return out
}

func paneLooksIdle(content string) bool {
	lines := strings.Split(strings.TrimRight(content, "\n"), "\n")
	checked := 0
	for i := len(lines) - 1; i >= 0; i-- {
		line := strings.TrimSpace(lines[i])
		if line == "" {
			continue
		}
		checked++
		if strings.HasSuffix(line, "❯") || line == "❯" || strings.HasPrefix(line, "›") || line == ">" || line == "$" || line == "#" || line == "claude>" {
			return true
		}
		if checked >= 4 {
			return false
		}
	}
	return false
}

func readTasksSnapshot(path string) ([]types.Task, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var tasks []types.Task
	if err := json.Unmarshal(data, &tasks); err != nil {
		return nil, err
	}
	sort.Slice(tasks, func(i, j int) bool {
		return tasks[i].ID < tasks[j].ID
	})
	return tasks, nil
}

func waitForCodexSessionDir(ctx context.Context, homeDir, sessionGlob string) (string, error) {
	pattern := filepath.Join(homeDir, ".ax", "codex", sessionGlob)
	for {
		select {
		case <-ctx.Done():
			return "", fmt.Errorf("timed out waiting for Codex session directory")
		default:
		}

		matches, err := filepath.Glob(pattern)
		if err != nil {
			return "", err
		}
		if len(matches) > 0 {
			sort.Strings(matches)
			return matches[len(matches)-1], nil
		}
		time.Sleep(500 * time.Millisecond)
	}
}

func historyContains(path, needle string) bool {
	data, err := os.ReadFile(path)
	if err != nil {
		return false
	}
	return strings.Contains(string(data), needle)
}

func repoRoot(t *testing.T) string {
	t.Helper()
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	return filepath.Dir(filepath.Dir(file))
}

func copyTree(t *testing.T, srcRoot, dstRoot string) {
	t.Helper()
	err := filepath.WalkDir(srcRoot, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(srcRoot, path)
		if err != nil {
			return err
		}
		target := filepath.Join(dstRoot, rel)
		if d.IsDir() {
			return os.MkdirAll(target, 0o755)
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		return os.WriteFile(target, data, 0o644)
	})
	if err != nil {
		t.Fatalf("copy fixture: %v", err)
	}
}

func requireTool(t *testing.T, name string) {
	t.Helper()
	if _, err := exec.LookPath(name); err != nil {
		t.Skipf("skipping live e2e because %s is not installed: %v", name, err)
	}
}

func mustMkdirAll(t *testing.T, path string) {
	t.Helper()
	if err := os.MkdirAll(path, 0o755); err != nil {
		t.Fatalf("mkdir %s: %v", path, err)
	}
}

func seedCodexBaseHome(t *testing.T, hostHome, sandboxHome string) {
	t.Helper()
	srcRoot := filepath.Join(hostHome, ".codex")
	authPath := filepath.Join(srcRoot, "auth.json")
	if _, err := os.Stat(authPath); err != nil {
		t.Skipf("skipping live e2e because %s is unavailable: %v", authPath, err)
	}

	dstRoot := filepath.Join(sandboxHome, ".codex")
	mustMkdirAll(t, dstRoot)
	// Intentionally do not link the host config.toml. The live e2e should
	// authenticate with the host account but run with only the sandboxed ax MCP
	// that PrepareCodexHome wires into the per-session Codex config.
	linkFileIntoSandbox(t, filepath.Join(srcRoot, "auth.json"), filepath.Join(dstRoot, "auth.json"))
}

func linkFileIntoSandbox(t *testing.T, src, dst string) {
	t.Helper()
	if _, err := os.Stat(src); err != nil {
		if os.IsNotExist(err) {
			return
		}
		t.Fatalf("stat %s: %v", src, err)
	}
	_ = os.Remove(dst)
	if err := os.Symlink(src, dst); err != nil {
		t.Fatalf("symlink %s -> %s: %v", dst, src, err)
	}
}

func runCommand(ctx context.Context, dir string, env []string, name string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, name, args...)
	if dir != "" {
		cmd.Dir = dir
	}
	if len(env) > 0 {
		cmd.Env = env
	}
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := cmd.Run()
	if err == nil {
		return stdout.String(), nil
	}
	var out strings.Builder
	if stdout.Len() > 0 {
		out.WriteString(stdout.String())
	}
	if stderr.Len() > 0 {
		if out.Len() > 0 {
			out.WriteString("\n")
		}
		out.WriteString(stderr.String())
	}
	return out.String(), fmt.Errorf("%s %s failed: %w\n%s", name, strings.Join(args, " "), err, strings.TrimSpace(out.String()))
}

func readFileBestEffort(path string) string {
	data, err := os.ReadFile(path)
	if err != nil {
		return fmt.Sprintf("read %s: %v", path, err)
	}
	return string(data)
}

func joinErr(left, right error) error {
	if left == nil {
		return right
	}
	if right == nil {
		return left
	}
	return errors.Join(left, right)
}

func filteredEnv(env []string, dropKeys ...string) []string {
	if len(env) == 0 {
		return nil
	}

	drop := make(map[string]struct{}, len(dropKeys))
	for _, key := range dropKeys {
		if key == "" {
			continue
		}
		drop[key] = struct{}{}
	}

	out := make([]string, 0, len(env))
	for _, entry := range env {
		key, _, ok := strings.Cut(entry, "=")
		if !ok {
			out = append(out, entry)
			continue
		}
		if _, shouldDrop := drop[key]; shouldDrop {
			continue
		}
		out = append(out, entry)
	}
	return out
}

func promptPrefix(prompt string, max int) string {
	head := strings.Split(prompt, "\n")[0]
	if len(head) <= max {
		return head
	}
	return head[:max]
}

func cleanupHostLeakedTmuxSessions(ctx context.Context, env []string, sandboxRoot string) error {
	sessions, err := leakedHostSessions(ctx, env, sandboxRoot)
	if err != nil {
		return err
	}
	for _, name := range sessions {
		if _, err := runCommand(ctx, "", env, "tmux", "kill-session", "-t", name); err != nil {
			return err
		}
	}
	return nil
}

func leakedHostSessions(ctx context.Context, env []string, sandboxRoot string) ([]string, error) {
	out, err := runCommand(ctx, "", env, "tmux", "list-panes", "-a", "-F", "#{session_name}|#{pane_current_path}")
	if err != nil {
		if strings.Contains(err.Error(), "no server running") {
			return nil, nil
		}
		return nil, err
	}
	return parseLeakedHostSessions(out, sandboxPathPrefixes(sandboxRoot)), nil
}

func parseLeakedHostSessions(out string, sandboxPrefixes []string) []string {
	seen := make(map[string]struct{})
	var sessions []string
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line == "" {
			continue
		}
		name, path, ok := strings.Cut(line, "|")
		if !ok {
			continue
		}
		if !hasAnyPrefix(path, sandboxPrefixes) {
			continue
		}
		if _, exists := seen[name]; exists {
			continue
		}
		seen[name] = struct{}{}
		sessions = append(sessions, name)
	}
	sort.Strings(sessions)
	return sessions
}

func sandboxPathPrefixes(root string) []string {
	seen := map[string]struct{}{}
	add := func(path string) {
		path = filepath.Clean(path)
		if path == "." || path == "" {
			return
		}
		if _, ok := seen[path]; ok {
			return
		}
		seen[path] = struct{}{}
	}

	add(root)
	if resolved, err := filepath.EvalSymlinks(root); err == nil {
		add(resolved)
	}
	if strings.HasPrefix(root, "/tmp/") {
		add(filepath.Join("/private", root))
	}

	prefixes := make([]string, 0, len(seen))
	for path := range seen {
		prefixes = append(prefixes, path)
	}
	sort.Strings(prefixes)
	return prefixes
}

func hasAnyPrefix(path string, prefixes []string) bool {
	path = filepath.Clean(path)
	for _, prefix := range prefixes {
		if strings.HasPrefix(path, prefix) {
			return true
		}
	}
	return false
}
