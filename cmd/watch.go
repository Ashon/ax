package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"regexp"
	"sort"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/spf13/cobra"
)

var watchCmd = &cobra.Command{
	Use:   "watch",
	Short: "Monitor workspace sessions with interactive TUI",
	RunE: func(cmd *cobra.Command, args []string) error {
		p := tea.NewProgram(newWatchModel(), tea.WithAltScreen())
		_, err := p.Run()
		return err
	},
}

// Styles
var (
	sidebarStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	selectedStyle   = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("6"))
	unselectedStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("7"))
	headerStyle     = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("6"))
	borderClr       = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	statStyle       = lipgloss.NewStyle().Foreground(lipgloss.Color("11"))
	msgBorderClr    = lipgloss.NewStyle().Foreground(lipgloss.Color("5"))
	msgTitleStyle   = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("5"))
	msgFromStyle    = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("3"))
	msgToStyle      = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("2"))
	msgTimeStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	runtimeStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("10"))
)

// Regex for parsing agent status
var (
	tokenUpRe   = regexp.MustCompile(`↑\s*([\d.]+[kKmM]?)\s*tokens`)
	tokenDownRe = regexp.MustCompile(`↓\s*([\d.]+[kKmM]?)\s*tokens`)
	costRe      = regexp.MustCompile(`\$[\d.]+`)
	agentStateRe = regexp.MustCompile(`(thinking|Harmonizing|Crystallizing|Nesting)`)
)

type tickMsg time.Time

const watchFPS = 60
const watchMessagePaneMinHeight = 6
const watchSidebarWidth = 34

type watchModel struct {
	width      int
	height     int
	selected   int
	captures   map[string]string
	prevCaps   map[string]string // previous tick captures for activity detection
	activity   map[string]time.Time // last activity time per workspace
	sessions   []tmux.SessionInfo
	runtimes   map[string]string
	msgHistory []daemon.HistoryEntry
	histPath   string
	showStream bool
}

type sidebarEntry struct {
	label        string
	sessionIndex int
	group        bool
	level        int
}

func newWatchModel() watchModel {
	return watchModel{
		captures:   make(map[string]string),
		prevCaps:   make(map[string]string),
		activity:   make(map[string]time.Time),
		runtimes:   loadWatchRuntimes(),
		histPath:   daemon.HistoryFilePath(socketPath),
		showStream: true,
	}
}

func (m watchModel) Init() tea.Cmd {
	return tea.Batch(tickCmd(), tea.WindowSize())
}

func (m watchModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "q", "ctrl+c":
			return m, tea.Quit
		case "up", "k":
			m.selected = moveSelection(m.selected, m.sessions, -1)
		case "down", "j":
			m.selected = moveSelection(m.selected, m.sessions, 1)
		case "tab":
			m.showStream = !m.showStream
		case "x":
			if m.selected < len(m.sessions) {
				_ = tmux.InterruptWorkspace(m.sessions[m.selected].Workspace)
			}
		}
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
	case tickMsg:
		m.sessions, _ = tmux.ListSessions()
		m.selected = clampSelection(m.selected, m.sessions)

		// Resize selected session's tmux window to match main panel
		if m.selected < len(m.sessions) && m.width > 0 {
			sideW := watchSidebarWidth
			mainW := m.width - sideW - 2 // inner content width
			streamH := messagePaneHeight(m.height, m.showStream)
			mainH := m.height - streamH - 3 // inner content height
			if mainW > 10 && mainH > 5 {
				selected := m.sessions[m.selected]
				resizeTmuxWindow(selected.Name, mainW, mainH)
			}
		}

		for _, s := range m.sessions {
			content := capturePane(s.Name)
			if prev, ok := m.prevCaps[s.Workspace]; ok && prev != content {
				m.activity[s.Workspace] = time.Now()
			}
			m.prevCaps[s.Workspace] = m.captures[s.Workspace]
			m.captures[s.Workspace] = content
		}
		m.msgHistory = readHistoryFile(m.histPath, 50)
		return m, tickCmd()
	}
	return m, nil
}

func (m watchModel) View() string {
	if m.width == 0 || len(m.sessions) == 0 {
		return "Loading... (waiting for sessions)"
	}

	// Layout: sidebar outerW + main outerW = total width
	sideW := watchSidebarWidth
	mainW := m.width - sideW
	if mainW < 20 {
		mainW = 20
	}

	streamH := messagePaneHeight(m.height, m.showStream)
	contentH := m.height - streamH - 1 // -1 for help line

	// === Sidebar ===
	sidebar := m.renderSidebar(sideW, contentH)

	// === Main pane ===
	var mainContent string
	if m.selected < len(m.sessions) {
		ws := m.sessions[m.selected].Workspace
		content := m.captures[ws]
		mainContent = m.renderMain(ws, content, mainW, contentH)
	}

	// Join sidebar + main
	top := lipgloss.JoinHorizontal(lipgloss.Top, sidebar, mainContent)

	// === Message stream ===
	var stream string
	if m.showStream {
		stream = m.renderStream(m.width, streamH)
	}

	// === Help line ===
	help := lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Render(
		" ↑↓ select · x interrupt · tab stream · q quit")

	parts := []string{top}
	if stream != "" {
		parts = append(parts, stream)
	}
	parts = append(parts, help)

	return lipgloss.JoinVertical(lipgloss.Left, parts...)
}

func (m watchModel) renderSidebar(w, h int) string {
	innerW := w - 2
	innerH := h - 2

	// Title
	title := headerStyle.Render(" agents ")
	titleW := lipgloss.Width(title)
	pad := innerW - titleW - 1
	if pad < 0 {
		pad = 0
	}
	topLine := borderClr.Render("╭─") + title + borderClr.Render(strings.Repeat("─", pad)+"╮")

	// Agent list
	actDot := lipgloss.NewStyle().Foreground(lipgloss.Color("2")).Render("●")
	idleDot := lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Render("○")

	var lines []string
	for _, entry := range buildSidebarEntries(m.sessions) {
		cursor := "  "
		left := ""
		right := ""

		if entry.group {
			left = sidebarStyle.Render(strings.Repeat("  ", entry.level) + entry.label)
		} else if entry.sessionIndex < 0 || entry.sessionIndex >= len(m.sessions) {
			// Workspace defined but not running
			dimStyle := lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
			left = "  " + strings.Repeat("  ", entry.level) + "○ " + dimStyle.Render(entry.label)
			right = dimStyle.Render("offline")
		} else {
			s := m.sessions[entry.sessionIndex]
			status := parseAgentStatus(m.captures[s.Workspace])
			runtime := m.runtimes[s.Workspace]

			dot := idleDot
			lastActive, hasActivity := m.activity[s.Workspace]
			if hasActivity && time.Since(lastActive) < 5*time.Second {
				dot = actDot
			}

			nameStyle := unselectedStyle
			if entry.sessionIndex == m.selected {
				cursor = selectedStyle.Render("▸ ")
				nameStyle = selectedStyle
			}

			left = cursor + strings.Repeat("  ", entry.level) + dot + " " + nameStyle.Render(entry.label)
			if runtime != "" {
				right = runtimeStyle.Render(runtime)
			}
			if status != "" {
				if right != "" {
					right += " "
				}
				right += statStyle.Render(status)
			}
		}

		leftW := lipgloss.Width(left)
		rightW := lipgloss.Width(right)
		gap := innerW - leftW - rightW
		if gap < 1 {
			gap = 1
			if leftW+1+rightW > innerW {
				right = ""
				gap = innerW - leftW
				if gap < 0 {
					gap = 0
				}
			}
		}

		lines = append(lines, borderClr.Render("│")+left+strings.Repeat(" ", gap)+right+borderClr.Render("│"))
	}

	// Fill remaining height
	for len(lines) < innerH {
		empty := strings.Repeat(" ", innerW)
		lines = append(lines, borderClr.Render("│")+empty+borderClr.Render("│"))
	}
	if len(lines) > innerH {
		lines = lines[:innerH]
	}

	botLine := borderClr.Render("╰" + strings.Repeat("─", innerW) + "╯")

	all := []string{topLine}
	all = append(all, lines...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

func (m watchModel) renderMain(ws, content string, w, h int) string {
	innerW := w - 2 // subtract left + right border
	innerH := h - 2

	// Title
	title := headerStyle.Render(fmt.Sprintf(" %s ", ws))
	titleW := lipgloss.Width(title)
	pad := innerW - titleW - 1
	if pad < 0 {
		pad = 0
	}
	topLine := borderClr.Render("╭─") + title + borderClr.Render(strings.Repeat("─", pad)+"╮")

	// Content
	lines := strings.Split(content, "\n")
	for len(lines) > 0 && strings.TrimSpace(lines[len(lines)-1]) == "" {
		lines = lines[:len(lines)-1]
	}
	if len(lines) > innerH {
		lines = lines[len(lines)-innerH:]
	}

	var bodyLines []string
	for i := 0; i < innerH; i++ {
		line := ""
		if i < len(lines) {
			line = lines[i]
		}
		visW := lipgloss.Width(line)
		if visW > innerW {
			line = ansiTruncate(line, innerW)
			visW = lipgloss.Width(line)
		}
		padding := innerW - visW
		if padding < 0 {
			padding = 0
		}
		bodyLines = append(bodyLines, borderClr.Render("│")+line+strings.Repeat(" ", padding)+borderClr.Render("│"))
	}

	botLine := borderClr.Render("╰" + strings.Repeat("─", innerW) + "╯")

	all := []string{topLine}
	all = append(all, bodyLines...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

func (m watchModel) renderStream(totalW, totalH int) string {
	innerW := totalW - 2
	innerH := totalH - 2
	if innerH < 1 {
		innerH = 1
	}

	title := msgTitleStyle.Render(" messages ")
	titleW := lipgloss.Width(title)
	pad := innerW - titleW - 1
	if pad < 0 {
		pad = 0
	}
	topLine := msgBorderClr.Render("╭─") + title + msgBorderClr.Render(strings.Repeat("─", pad)+"╮")

	var msgLines []string
	start := 0
	if len(m.msgHistory) > innerH {
		start = len(m.msgHistory) - innerH
	}
	for _, entry := range m.msgHistory[start:] {
		ts := msgTimeStyle.Render(entry.Timestamp.Format("15:04:05"))
		from := msgFromStyle.Render(entry.From)
		to := msgToStyle.Render(entry.To)
		content := strings.ReplaceAll(entry.Content, "\n", " ")
		content = truncateStr(content, innerW-30)
		msgLines = append(msgLines, fmt.Sprintf(" %s %s → %s: %s", ts, from, to, content))
	}
	if len(msgLines) == 0 {
		msgLines = append(msgLines, msgTimeStyle.Render("  (no messages yet)"))
	}

	var bodyLines []string
	for i := 0; i < innerH; i++ {
		line := ""
		if i < len(msgLines) {
			line = msgLines[i]
		}
		visW := lipgloss.Width(line)
		if visW > innerW {
			line = truncateStr(line, innerW)
			visW = lipgloss.Width(line)
		}
		padding := innerW - visW
		if padding < 0 {
			padding = 0
		}
		bodyLines = append(bodyLines, msgBorderClr.Render("│")+line+strings.Repeat(" ", padding)+msgBorderClr.Render("│"))
	}

	botLine := msgBorderClr.Render("╰" + strings.Repeat("─", innerW) + "╯")

	all := []string{topLine}
	all = append(all, bodyLines...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

// Helpers

func tickCmd() tea.Cmd {
	return tea.Tick(time.Second/watchFPS, func(t time.Time) tea.Msg {
		return tickMsg(t)
	})
}

func messagePaneHeight(totalHeight int, showStream bool) int {
	if !showStream {
		return 0
	}

	streamH := totalHeight / 3
	if streamH < watchMessagePaneMinHeight {
		streamH = watchMessagePaneMinHeight
	}

	maxStreamH := totalHeight - 8
	if maxStreamH < watchMessagePaneMinHeight {
		maxStreamH = watchMessagePaneMinHeight
	}
	if streamH > maxStreamH {
		streamH = maxStreamH
	}

	return streamH
}

type sidebarTreeNode struct {
	name         string
	sessionIndex int
	children     map[string]*sidebarTreeNode
}

func buildSidebarEntries(sessions []tmux.SessionInfo) []sidebarEntry {
	// Try config-driven tree first; fall back to name-based splitting
	// when no config is available.
	if cfgPath, err := resolveConfigPath(); err == nil {
		if tree, err := config.LoadTree(cfgPath); err == nil && tree != nil {
			return buildSidebarFromTree(tree, sessions)
		}
	}

	root := &sidebarTreeNode{
		sessionIndex: -1,
		children:     make(map[string]*sidebarTreeNode),
	}

	for i, session := range sessions {
		node := root
		for _, part := range splitWorkspacePath(session.Workspace) {
			child, ok := node.children[part]
			if !ok {
				child = &sidebarTreeNode{
					name:         part,
					sessionIndex: -1,
					children:     make(map[string]*sidebarTreeNode),
				}
				node.children[part] = child
			}
			node = child
		}
		node.sessionIndex = i
	}

	var entries []sidebarEntry
	appendSidebarEntries(root, 0, &entries)
	return entries
}

// buildSidebarFromTree renders a project tree into sidebar entries.
// Each project becomes a group header. Its orchestrator is the first
// leaf under it, followed by workspaces, then nested projects.
func buildSidebarFromTree(tree *config.ProjectNode, sessions []tmux.SessionInfo) []sidebarEntry {
	sessionByWorkspace := make(map[string]int, len(sessions))
	for i, s := range sessions {
		sessionByWorkspace[s.Workspace] = i
	}

	var entries []sidebarEntry
	appendProjectEntries(tree, 0, sessionByWorkspace, &entries)
	return entries
}

func appendProjectEntries(node *config.ProjectNode, level int, sessionByWorkspace map[string]int, entries *[]sidebarEntry) {
	if node == nil {
		return
	}

	*entries = append(*entries, sidebarEntry{
		label: "▾ " + node.Name,
		group: true,
		level: level,
	})

	// Project orchestrator first
	orchName := "orchestrator"
	if node.Prefix != "" {
		orchName = node.Prefix + ".orchestrator"
	}
	orchLabel := "◆ orchestrator"
	if idx, ok := sessionByWorkspace[orchName]; ok {
		*entries = append(*entries, sidebarEntry{
			label:        orchLabel,
			sessionIndex: idx,
			level:        level + 1,
		})
	} else {
		*entries = append(*entries, sidebarEntry{
			label:        orchLabel,
			sessionIndex: -1,
			level:        level + 1,
		})
	}

	for _, ws := range node.Workspaces {
		idx, ok := sessionByWorkspace[ws.MergedName]
		if !ok {
			*entries = append(*entries, sidebarEntry{
				label:        ws.Name,
				sessionIndex: -1,
				level:        level + 1,
			})
			continue
		}
		*entries = append(*entries, sidebarEntry{
			label:        ws.Name,
			sessionIndex: idx,
			level:        level + 1,
		})
	}

	for _, child := range node.Children {
		appendProjectEntries(child, level+1, sessionByWorkspace, entries)
	}
}

func appendSidebarEntries(node *sidebarTreeNode, level int, entries *[]sidebarEntry) {
	childNames := make([]string, 0, len(node.children))
	for name := range node.children {
		childNames = append(childNames, name)
	}
	sort.Strings(childNames)

	for _, name := range childNames {
		child := node.children[name]

		if len(child.children) > 0 {
			*entries = append(*entries, sidebarEntry{
				label: "▾ " + child.name,
				group: true,
				level: level,
			})
		}
		if child.sessionIndex >= 0 {
			*entries = append(*entries, sidebarEntry{
				label:        child.name,
				sessionIndex: child.sessionIndex,
				level:        level,
			})
		}
		if len(child.children) > 0 {
			appendSidebarEntries(child, level+1, entries)
		}
	}
}

func splitWorkspacePath(workspace string) []string {
	switch {
	case strings.Contains(workspace, "."):
		return strings.Split(workspace, ".")
	case strings.Count(workspace, "_") >= 2:
		return strings.Split(workspace, "_")
	default:
		return []string{workspace}
	}
}

func loadWatchRuntimes() map[string]string {
	runtimes := map[string]string{
		"orchestrator": agent.NormalizeRuntime(""),
	}

	cfgPath, err := resolveConfigPath()
	if err != nil {
		return runtimes
	}
	cfg, err := config.Load(cfgPath)
	if err != nil {
		return runtimes
	}

	runtimes["orchestrator"] = agent.NormalizeRuntime(cfg.OrchestratorRuntime)
	for name, ws := range cfg.Workspaces {
		runtime := agent.NormalizeRuntime(ws.Runtime)
		runtimes[name] = runtime
		runtimes[strings.ReplaceAll(name, ".", "_")] = runtime
	}
	return runtimes
}

func orderedLeafSessionIndices(sessions []tmux.SessionInfo) []int {
	entries := buildSidebarEntries(sessions)
	indices := make([]int, 0, len(sessions))
	for _, entry := range entries {
		if !entry.group && entry.sessionIndex >= 0 {
			indices = append(indices, entry.sessionIndex)
		}
	}
	return indices
}

func moveSelection(current int, sessions []tmux.SessionInfo, delta int) int {
	indices := orderedLeafSessionIndices(sessions)
	if len(indices) == 0 {
		return 0
	}

	pos := 0
	for i, idx := range indices {
		if idx == current {
			pos = i
			break
		}
	}

	pos += delta
	if pos < 0 {
		pos = 0
	}
	if pos >= len(indices) {
		pos = len(indices) - 1
	}
	return indices[pos]
}

func clampSelection(current int, sessions []tmux.SessionInfo) int {
	indices := orderedLeafSessionIndices(sessions)
	if len(indices) == 0 {
		return 0
	}
	for _, idx := range indices {
		if idx == current {
			return current
		}
	}
	return indices[0]
}

func resizeTmuxWindow(sessionName string, w, h int) {
	exec.Command("tmux", "resize-window", "-t", sessionName,
		"-x", fmt.Sprintf("%d", w),
		"-y", fmt.Sprintf("%d", h),
	).Run()
}

func capturePane(sessionName string) string {
	out, err := exec.Command("tmux", "capture-pane", "-t", sessionName, "-p", "-e").Output()
	if err != nil {
		return "(capture failed)"
	}
	return string(out)
}

func parseAgentStatus(content string) string {
	lines := strings.Split(content, "\n")
	start := len(lines) - 30
	if start < 0 {
		start = 0
	}

	var up, down, cost, status string
	for _, line := range lines[start:] {
		if m := tokenUpRe.FindStringSubmatch(line); m != nil {
			up = "↑" + m[1]
		}
		if m := tokenDownRe.FindStringSubmatch(line); m != nil {
			down = "↓" + m[1]
		}
		if m := costRe.FindString(line); m != "" {
			cost = m
		}
		if m := agentStateRe.FindString(line); m != "" {
			status = m
		}
	}

	var parts []string
	if up != "" || down != "" {
		parts = append(parts, strings.TrimSpace(up+" "+down))
	}
	if cost != "" {
		parts = append(parts, cost)
	}
	if status != "" {
		parts = append(parts, status)
	}
	if len(parts) == 0 {
		return ""
	}
	return strings.Join(parts, " ")
}

func ansiTruncate(s string, maxW int) string {
	visW := 0
	i := 0
	runes := []rune(s)
	for i < len(runes) {
		if runes[i] == '\x1b' {
			j := i + 1
			if j < len(runes) && runes[j] == '[' {
				j++
				for j < len(runes) && !((runes[j] >= 'A' && runes[j] <= 'Z') || (runes[j] >= 'a' && runes[j] <= 'z')) {
					j++
				}
				if j < len(runes) {
					j++
				}
			}
			i = j
			continue
		}
		visW++
		if visW > maxW {
			return string(runes[:i])
		}
		i++
	}
	return s
}

func readHistoryFile(path string, maxEntries int) []daemon.HistoryEntry {
	f, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer f.Close()

	var entries []daemon.HistoryEntry
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1024*1024), 1024*1024)
	for scanner.Scan() {
		var entry daemon.HistoryEntry
		if json.Unmarshal(scanner.Bytes(), &entry) == nil {
			entries = append(entries, entry)
		}
	}
	if len(entries) > maxEntries {
		entries = entries[len(entries)-maxEntries:]
	}
	return entries
}

func truncateStr(s string, n int) string {
	if n <= 0 {
		return ""
	}
	r := []rune(s)
	if len(r) <= n {
		return s
	}
	return string(r[:n]) + "…"
}

func init() {
	rootCmd.AddCommand(watchCmd)
}
