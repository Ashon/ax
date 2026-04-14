package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"
	"unicode"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/usage"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	xansi "github.com/charmbracelet/x/ansi"
	"github.com/spf13/cobra"
)

var watchCmd = &cobra.Command{
	Use:   "watch",
	Short: "Monitor workspace sessions with interactive TUI",
	RunE: func(cmd *cobra.Command, args []string) error {
		initialStream, streamOnly, err := resolveWatchInitialView(watchShowAgents, watchShowTasks, watchShowMessages, watchShowTokens)
		if err != nil {
			return err
		}
		p := tea.NewProgram(newWatchModel(initialStream, streamOnly), tea.WithAltScreen())
		_, err = p.Run()
		return err
	},
}

var (
	watchShowAgents   bool
	watchShowTasks    bool
	watchShowMessages bool
	watchShowTokens   bool
)

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
	tokenSidebarSty = lipgloss.NewStyle().Foreground(lipgloss.Color("13"))
	footerSummarySt = lipgloss.NewStyle().Foreground(lipgloss.Color("13"))
	taskBorderClr   = lipgloss.NewStyle().Foreground(lipgloss.Color("4"))
	taskTitleStyle  = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("4"))
	taskPendingClr  = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	taskActiveClr   = lipgloss.NewStyle().Foreground(lipgloss.Color("11"))
	taskDoneClr     = lipgloss.NewStyle().Foreground(lipgloss.Color("2"))
	taskFailClr     = lipgloss.NewStyle().Foreground(lipgloss.Color("1"))
)

type streamView int

const (
	streamMessages streamView = iota
	streamTasks
	streamTokens
	streamHidden
)

const streamViewCount = 4

type agentTokens struct {
	Workspace string
	Up        string // raw string like "123.4k"
	Down      string // raw string like "45.6k"
	Total     string // raw string like "93.9k" from total-only done lines
	Cost      string // raw string like "$1.23"
}

// Regex for parsing agent status
var (
	tokenUpRe        = regexp.MustCompile(`↑\s*([\d.]+[kKmM]?)\s*tokens`)
	tokenDownRe      = regexp.MustCompile(`↓\s*([\d.]+[kKmM]?)\s*tokens`)
	tokenAnyRe       = regexp.MustCompile(`([\d.]+[kKmM]?)\s*tokens`)
	costRe           = regexp.MustCompile(`\$[\d.]+`)
	agentStateRe     = regexp.MustCompile(`(?i)(thinking|Harmonizing|Crystallizing|Nesting)`)
	claudeDoneLineRe = regexp.MustCompile(`\bDone \(`)
)

type tickMsg time.Time

const watchFPS = 60
const watchMessagePaneMinHeight = 6
const watchSidebarWidth = 34
const watchDataRefreshInterval = 250 * time.Millisecond
const watchSessionRefreshInterval = time.Second
const watchBackgroundCaptureBatchSize = 4
const sidebarRecentActivityWindow = 5 * time.Second
const watchWorkspaceRefreshInterval = time.Second
const watchTrendRefreshInterval = 15 * time.Second
const watchTrendWindowMinutes = 24 * 60
const watchTrendBucketMinutes = 3 * 60

var (
	sidebarRunningFrames = []string{"⠁", "⠃", "⠇", "⠧", "⠷", "⠿", "⠷", "⠧", "⠇", "⠃"}
)

const (
	sidebarAgentStateOffline = "offline"
	sidebarAgentStateIdle    = "idle"
	sidebarAgentStateRunning = "running"
)

type watchModel struct {
	width                  int
	height                 int
	spinnerTick            int
	dataRefreshedAt        time.Time
	sessionsRefreshedAt    time.Time
	forceDataRefresh       bool
	selected               int
	taskSelected           int
	taskFilter             taskFilterMode
	captureCursor          int
	captures               map[string]string
	prevCaps               map[string]string    // previous tick captures for activity detection
	activity               map[string]time.Time // last activity time per workspace
	sessions               []tmux.SessionInfo
	runtimes               map[string]string
	msgHistory             []daemon.HistoryEntry
	histPath               string
	historyFileModTime     time.Time
	tasks                  []types.Task
	tasksPath              string
	tasksFileModTime       time.Time
	tokenData              map[string]agentTokens
	trendData              map[string]usage.WorkspaceTrend
	sidebarStates          map[string]string
	workspaceInfos         map[string]types.WorkspaceInfo
	workspaceInfoUpdatedAt time.Time
	trendUpdatedAt         time.Time
	stream                 streamView
	streamOnly             bool
	mainResize             tmuxResizeState
}

type sidebarEntry struct {
	label        string
	workspace    string
	sessionIndex int
	group        bool
	level        int
}

type tmuxResizeState struct {
	sessionName string
	width       int
	height      int
}

func newWatchModel(initialStream streamView, streamOnly bool) watchModel {
	return watchModel{
		captures:         make(map[string]string),
		prevCaps:         make(map[string]string),
		activity:         make(map[string]time.Time),
		runtimes:         loadWatchRuntimes(),
		histPath:         daemon.HistoryFilePath(socketPath),
		tasksPath:        daemon.TasksFilePath(socketPath),
		tokenData:        make(map[string]agentTokens),
		trendData:        make(map[string]usage.WorkspaceTrend),
		sidebarStates:    make(map[string]string),
		workspaceInfos:   make(map[string]types.WorkspaceInfo),
		stream:           initialStream,
		streamOnly:       streamOnly,
		taskFilter:       taskFilterActive,
		taskSelected:     0,
		forceDataRefresh: true,
	}
}

func resolveWatchInitialView(agents, tasks, messages, tokens bool) (streamView, bool, error) {
	selectionCount := 0
	selected := streamMessages
	streamOnly := false

	if agents {
		selectionCount++
		selected = streamHidden
	}
	if tasks {
		selectionCount++
		selected = streamTasks
		streamOnly = true
	}
	if messages {
		selectionCount++
		selected = streamMessages
		streamOnly = true
	}
	if tokens {
		selectionCount++
		selected = streamTokens
		streamOnly = true
	}

	if selectionCount > 1 {
		return streamMessages, false, fmt.Errorf("watch view flags are mutually exclusive; use only one of --agents, --tasks, --messages, or --tokens")
	}
	return selected, streamOnly, nil
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
			m.forceDataRefresh = true
		case "down", "j":
			m.selected = moveSelection(m.selected, m.sessions, 1)
			m.forceDataRefresh = true
		case "tab":
			if m.streamOnly {
				switch m.stream {
				case streamMessages:
					m.stream = streamTasks
				case streamTasks:
					m.stream = streamTokens
				default:
					m.stream = streamMessages
				}
			} else {
				m.stream = (m.stream + 1) % streamViewCount
			}
		case "[", "H":
			m.taskSelected = moveTaskSelection(m.taskSelected, m.tasks, m.taskFilter, -1)
		case "]", "L":
			m.taskSelected = moveTaskSelection(m.taskSelected, m.tasks, m.taskFilter, 1)
		case "f":
			m.taskFilter = nextTaskFilterMode(m.taskFilter)
			m.taskSelected = clampTaskSelection(m.taskSelected, m.tasks, m.taskFilter)
		case "x":
			if m.selected < len(m.sessions) {
				_ = tmux.InterruptWorkspace(m.sessions[m.selected].Workspace)
			}
		}
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		m.forceDataRefresh = true
	case tickMsg:
		m.spinnerTick++
		now := time.Time(msg)
		if !watchShouldRefreshData(m.dataRefreshedAt, now, m.forceDataRefresh) {
			return m, tickCmd()
		}
		if watchShouldRefreshSessions(m.sessionsRefreshedAt, now, len(m.sessions) == 0) {
			m.sessions, _ = tmux.ListSessions()
			m.sessionsRefreshedAt = now
		}
		m.selected = clampSelection(m.selected, m.sessions)

		// Resize selected session's tmux window to match main panel
		if !m.streamOnly && m.selected < len(m.sessions) && m.width > 0 {
			sideW := watchSidebarWidth
			mainW := m.width - sideW - 2 // inner content width
			streamH := streamPaneHeight(m.height, m.stream)
			mainH := m.height - streamH - 3 // inner content height
			if mainW > 10 && mainH > 5 {
				selected := m.sessions[m.selected]
				resizeTmuxWindowIfNeeded(&m.mainResize, selected.Name, mainW, mainH)
			}
		}

		targets, nextCursor := planCaptureTargets(m.sessions, watchFocusedWorkspaces(m.sessions, m.selected), m.captureCursor, watchBackgroundCaptureBatchSize)
		m.captureCursor = nextCursor
		refreshSessionCaptures(targets, m.captures, m.prevCaps, m.activity, m.sidebarStates, m.tokenData, now)

		m.msgHistory, m.historyFileModTime = readHistoryFileIfChanged(m.histPath, m.historyFileModTime, m.msgHistory, 50)
		m.tasks, m.tasksFileModTime = readTasksFileIfChanged(m.tasksPath, m.tasksFileModTime, m.tasks)
		m.workspaceInfos, m.workspaceInfoUpdatedAt = refreshWatchWorkspaceInfos(m.workspaceInfos, m.workspaceInfoUpdatedAt)
		m.trendData, m.trendUpdatedAt = refreshWatchTokenTrends(m.trendData, m.trendUpdatedAt, m.sessions, now, m.forceDataRefresh)
		m.taskSelected = clampTaskSelection(m.taskSelected, m.tasks, m.taskFilter)
		m.dataRefreshedAt = now
		m.forceDataRefresh = false
		return m, tickCmd()
	}
	return m, nil
}

func (m watchModel) View() string {
	if m.width == 0 || len(m.sessions) == 0 {
		return "Loading... (waiting for sessions)"
	}

	footer := m.renderFooter(m.width)
	if m.streamOnly {
		contentH := m.height - lipgloss.Height(footer)
		if contentH < watchMessagePaneMinHeight {
			contentH = watchMessagePaneMinHeight
		}
		return lipgloss.JoinVertical(lipgloss.Left, m.renderSelectedStream(m.width, contentH), footer)
	}

	// Layout: sidebar outerW + main outerW = total width
	sideW := watchSidebarWidth
	mainW := m.width - sideW
	if mainW < 20 {
		mainW = 20
	}

	streamH := streamPaneHeight(m.height, m.stream)
	contentH := m.height - streamH - lipgloss.Height(footer)

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

	// === Stream pane (messages, tasks, or tokens) ===
	stream := m.renderSelectedStream(m.width, streamH)

	parts := []string{top}
	if stream != "" {
		parts = append(parts, stream)
	}
	parts = append(parts, footer)

	return lipgloss.JoinVertical(lipgloss.Left, parts...)
}

func (m watchModel) renderFooter(totalW int) string {
	summary := footerSummarySt.Render(fitDisplayText(m.footerTokenSummary(totalW), totalW))
	helpText := " ↑↓ agent · [/ ] task · f filter · x interrupt · tab msgs/tasks/tokens/off · q quit"
	if m.streamOnly {
		helpText = " [/ ] task · f filter · tab msgs/tasks/tokens · q quit"
	}
	help := lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Render(
		fitDisplayText(helpText, totalW))
	return lipgloss.JoinVertical(lipgloss.Left, summary, help)
}

func (m watchModel) renderSelectedStream(totalW, totalH int) string {
	switch m.stream {
	case streamMessages:
		return m.renderStream(totalW, totalH)
	case streamTasks:
		return m.renderTasks(totalW, totalH)
	case streamTokens:
		return m.renderTokens(totalW, totalH)
	default:
		return ""
	}
}

func (m watchModel) renderSidebar(w, h int) string {
	innerW := w - 2
	innerH := h - 2
	attentionByWorkspace := summarizeWorkspaceAttention(m.tasks)

	// Title
	title := headerStyle.Render(" agents ")
	titleW := lipgloss.Width(title)
	pad := innerW - titleW - 1
	if pad < 0 {
		pad = 0
	}
	topLine := borderClr.Render("╭─") + title + borderClr.Render(strings.Repeat("─", pad)+"╮")

	var lines []string
	stateNow := m.dataRefreshedAt
	if stateNow.IsZero() {
		stateNow = time.Now()
	}
	for _, entry := range buildSidebarEntries(m.sessions) {
		if entry.group {
			left := sidebarStyle.Render(strings.Repeat("  ", entry.level) + entry.label)
			lines = append(lines, renderWatchSidebarLine(left, "", innerW))
			continue
		}

		workspaceName := entry.workspace
		if workspaceName == "" && entry.sessionIndex >= 0 && entry.sessionIndex < len(m.sessions) {
			workspaceName = m.sessions[entry.sessionIndex].Workspace
		}
		attention := workspaceAttentionBadge(attentionByWorkspace[workspaceName])
		statusText := workspaceStatusPreview(m.workspaceInfos, workspaceName, max(0, innerW-6))
		cursor := "  "
		left := ""
		secondary := ""
		right := ""

		if entry.sessionIndex < 0 || entry.sessionIndex >= len(m.sessions) {
			// Workspace defined but not running
			dimStyle := lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
			left = "  " + strings.Repeat("  ", entry.level) + renderSidebarStateMarker(sidebarAgentStateOffline, m.spinnerTick) + " " + dimStyle.Render(entry.label)
			right = formatSidebarRowMeta("offline", "", attention, max(0, innerW-lipgloss.Width(left)-1))
		} else {
			s := m.sessions[entry.sessionIndex]
			agentStatus := parseAgentStatus(m.captures[s.Workspace])
			runtime := m.runtimes[s.Workspace]
			state := m.sidebarStates[s.Workspace]
			if state == "" {
				state = deriveSidebarAgentState(m.captures[s.Workspace], m.activity[s.Workspace], stateNow)
			}
			marker := renderSidebarStateMarker(state, m.spinnerTick)

			nameStyle := unselectedStyle
			if entry.sessionIndex == m.selected {
				cursor = selectedStyle.Render("▸ ")
				nameStyle = selectedStyle
			}

			left = cursor + strings.Repeat("  ", entry.level) + marker + " " + nameStyle.Render(entry.label)
			tokenSummary := formatSidebarTokenSummary(m.tokenData[s.Workspace], max(0, innerW-lipgloss.Width(left)-1))
			right = formatSidebarRowMeta(runtime, tokenSummary, attention, max(0, innerW-lipgloss.Width(left)-1))
			secondary = statusText
			if secondary == "" {
				secondary = agentStatus
			}
		}

		lines = append(lines, renderWatchSidebarLine(left, right, innerW))
		if secondary != "" && entry.sessionIndex == m.selected {
			prefix := "    " + strings.Repeat("  ", entry.level)
			secondaryLine := prefix + fitDisplayText(secondary, max(0, innerW-lipgloss.Width(prefix)))
			lines = append(lines, renderWatchSidebarLine(selectedStyle.Render(secondaryLine), "", innerW))
		}
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
	for i := range lines {
		lines[i] = sanitizeDisplayLine(lines[i])
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
			line = xansi.Truncate(line, innerW, "")
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

func streamPaneHeight(totalHeight int, sv streamView) int {
	if sv == streamHidden {
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
// Running sessions not in the tree are appended under an "unregistered"
// group so they stay visible.
func buildSidebarFromTree(tree *config.ProjectNode, sessions []tmux.SessionInfo) []sidebarEntry {
	sessionByWorkspace := make(map[string]int, len(sessions))
	for i, s := range sessions {
		sessionByWorkspace[s.Workspace] = i
	}

	known := make(map[string]bool)
	collectKnownFromTree(tree, known)

	var entries []sidebarEntry
	appendProjectEntries(tree, 0, sessionByWorkspace, &entries)

	// Append any running session that wasn't part of the config tree
	var unregistered []int
	for i, s := range sessions {
		if !known[s.Workspace] {
			unregistered = append(unregistered, i)
		}
	}
	if len(unregistered) > 0 {
		entries = append(entries, sidebarEntry{
			label: "▾ unregistered",
			group: true,
			level: 0,
		})
		for _, idx := range unregistered {
			entries = append(entries, sidebarEntry{
				label:        sessions[idx].Workspace,
				sessionIndex: idx,
				level:        1,
			})
		}
	}

	return entries
}

func collectKnownFromTree(node *config.ProjectNode, known map[string]bool) {
	if node == nil {
		return
	}
	orchName := "orchestrator"
	if node.Prefix != "" {
		orchName = node.Prefix + ".orchestrator"
	}
	known[orchName] = true
	for _, ws := range node.Workspaces {
		known[ws.MergedName] = true
	}
	for _, child := range node.Children {
		collectKnownFromTree(child, known)
	}
}

func appendProjectEntries(node *config.ProjectNode, level int, sessionByWorkspace map[string]int, entries *[]sidebarEntry) {
	if node == nil {
		return
	}

	*entries = append(*entries, sidebarEntry{
		label: "▾ " + node.DisplayName(),
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

func loadWatchWorkspaceInfos() (map[string]types.WorkspaceInfo, bool) {
	sp := daemon.ExpandSocketPath(socketPath)
	if !isDaemonRunning(sp) {
		return map[string]types.WorkspaceInfo{}, true
	}
	client, err := newCLIClient()
	if err != nil {
		return nil, false
	}
	defer client.Close()

	workspaces, err := client.ListWorkspaces()
	if err != nil {
		return nil, false
	}
	return workspaceInfoMap(workspaces), true
}

func refreshWatchWorkspaceInfos(current map[string]types.WorkspaceInfo, last time.Time) (map[string]types.WorkspaceInfo, time.Time) {
	if !last.IsZero() && time.Since(last) < watchWorkspaceRefreshInterval {
		return current, last
	}
	next, ok := loadWatchWorkspaceInfos()
	now := time.Now()
	if !ok {
		return current, now
	}
	return next, now
}

func loadWatchWorkspaceDirs() map[string]string {
	dirs := make(map[string]string)
	if cwd, err := os.Getwd(); err == nil {
		dirs["orchestrator"] = cwd
	}

	cfgPath, err := resolveConfigPath()
	if err != nil {
		return dirs
	}
	cfg, err := config.Load(cfgPath)
	if err != nil {
		return dirs
	}
	for name, ws := range cfg.Workspaces {
		dirs[name] = ws.Dir
	}
	if _, ok := dirs["orchestrator"]; !ok {
		dirs["orchestrator"] = watchConfigRootDir(cfgPath)
	}
	return dirs
}

func watchConfigRootDir(cfgPath string) string {
	cfgPath = filepath.Clean(cfgPath)
	if filepath.Base(cfgPath) == config.LegacyConfigFile {
		return filepath.Dir(cfgPath)
	}
	if filepath.Base(cfgPath) == config.DefaultConfigFile && filepath.Base(filepath.Dir(cfgPath)) == config.DefaultConfigDir {
		return filepath.Dir(filepath.Dir(cfgPath))
	}
	return filepath.Dir(cfgPath)
}

func loadWatchTokenTrends(sessions []tmux.SessionInfo) (map[string]usage.WorkspaceTrend, bool) {
	sp := daemon.ExpandSocketPath(socketPath)
	if !isDaemonRunning(sp) {
		return map[string]usage.WorkspaceTrend{}, true
	}

	dirByWorkspace := loadWatchWorkspaceDirs()
	requests := make([]daemon.UsageTrendWorkspace, 0, len(sessions))
	seen := make(map[string]struct{}, len(sessions))
	for _, session := range sessions {
		if _, ok := seen[session.Workspace]; ok {
			continue
		}
		seen[session.Workspace] = struct{}{}
		cwd := strings.TrimSpace(dirByWorkspace[session.Workspace])
		if cwd == "" {
			continue
		}
		requests = append(requests, daemon.UsageTrendWorkspace{
			Workspace: session.Workspace,
			Cwd:       cwd,
		})
	}
	if len(requests) == 0 {
		return map[string]usage.WorkspaceTrend{}, true
	}

	client, err := newCLIClient()
	if err != nil {
		return nil, false
	}
	defer client.Close()

	trends, err := client.GetUsageTrends(requests, watchTrendWindowMinutes, watchTrendBucketMinutes)
	if err != nil {
		return nil, false
	}
	result := make(map[string]usage.WorkspaceTrend, len(trends))
	for _, trend := range trends {
		result[trend.Workspace] = trend
	}
	return result, true
}

func refreshWatchTokenTrends(current map[string]usage.WorkspaceTrend, last time.Time, sessions []tmux.SessionInfo, now time.Time, force bool) (map[string]usage.WorkspaceTrend, time.Time) {
	if !last.IsZero() && !force && now.Sub(last) < watchTrendRefreshInterval {
		return current, last
	}
	next, ok := loadWatchTokenTrends(sessions)
	if !ok {
		return current, now
	}
	return next, now
}

type workspaceAttention struct {
	Stale    int
	Diverged int
	Queued   int
}

type tokenTotals struct {
	ReportingAgents int
	SessionCount    int
	TotalUp         float64
	TotalDown       float64
	StandaloneTotal float64
	TotalCost       float64
	CostAgents      int
}

func summarizeWorkspaceAttention(tasks []types.Task) map[string]workspaceAttention {
	attentionByWorkspace := make(map[string]workspaceAttention)
	for _, task := range tasks {
		if task.Assignee == "" {
			continue
		}
		attention := attentionByWorkspace[task.Assignee]
		if taskIsStale(task) {
			attention.Stale++
		}
		if task.StaleInfo != nil {
			if task.StaleInfo.StateDivergence {
				attention.Diverged++
			}
			attention.Queued += task.StaleInfo.PendingMessages
		}
		attentionByWorkspace[task.Assignee] = attention
	}
	return attentionByWorkspace
}

func workspaceAttentionBadge(attention workspaceAttention) string {
	var parts []string
	if attention.Diverged > 0 {
		parts = append(parts, fmt.Sprintf("D%d", attention.Diverged))
	}
	if attention.Stale > 0 {
		parts = append(parts, fmt.Sprintf("S%d", attention.Stale))
	}
	if len(parts) == 0 && attention.Queued > 0 {
		parts = append(parts, fmt.Sprintf("Q%d", attention.Queued))
	}
	return strings.Join(parts, " ")
}

func taskAttentionSummary(task types.Task) string {
	var parts []string
	if task.StaleInfo != nil && task.StaleInfo.StateDivergence {
		parts = append(parts, "DIVERGED")
	}
	if taskIsStale(task) {
		parts = append(parts, "STALE")
	}
	if task.StaleInfo != nil && task.StaleInfo.PendingMessages > 0 {
		parts = append(parts, fmt.Sprintf("Q%d", task.StaleInfo.PendingMessages))
	}
	return strings.Join(parts, " ")
}

func renderWatchSidebarLine(left, right string, innerW int) string {
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
	return borderClr.Render("│") + left + strings.Repeat(" ", gap) + right + borderClr.Render("│")
}

func resizeTmuxWindowIfNeeded(state *tmuxResizeState, sessionName string, w, h int) {
	if sessionName == "" || w <= 0 || h <= 0 {
		return
	}
	if state.sessionName == sessionName && state.width == w && state.height == h {
		return
	}
	resizeTmuxWindow(sessionName, w, h)
	state.sessionName = sessionName
	state.width = w
	state.height = h
}

func watchShouldRefreshData(last, now time.Time, force bool) bool {
	if force || last.IsZero() {
		return true
	}
	return now.Sub(last) >= watchDataRefreshInterval
}

func watchShouldRefreshSessions(last, now time.Time, force bool) bool {
	if force || last.IsZero() {
		return true
	}
	return now.Sub(last) >= watchSessionRefreshInterval
}

func watchFocusedWorkspaces(sessions []tmux.SessionInfo, selected int) map[string]bool {
	focused := make(map[string]bool, 1)
	if selected >= 0 && selected < len(sessions) {
		focused[sessions[selected].Workspace] = true
	}
	return focused
}

func planCaptureTargets(sessions []tmux.SessionInfo, focused map[string]bool, cursor, batchSize int) ([]tmux.SessionInfo, int) {
	targets := make([]tmux.SessionInfo, 0, min(len(sessions), len(focused)+batchSize))
	background := make([]tmux.SessionInfo, 0, len(sessions))
	for _, session := range sessions {
		if focused[session.Workspace] {
			targets = append(targets, session)
			continue
		}
		background = append(background, session)
	}
	if len(background) == 0 || batchSize <= 0 {
		return targets, 0
	}
	if cursor < 0 || cursor >= len(background) {
		cursor = 0
	}
	count := min(batchSize, len(background))
	for i := 0; i < count; i++ {
		idx := (cursor + i) % len(background)
		targets = append(targets, background[idx])
	}
	return targets, (cursor + count) % len(background)
}

func refreshSessionCaptures(targets []tmux.SessionInfo, captures, prevCaps map[string]string, activity map[string]time.Time, sidebarStates map[string]string, tokenData map[string]agentTokens, now time.Time) {
	for _, session := range targets {
		content := capturePane(session.Name)
		previous := captures[session.Workspace]
		if previous != "" && previous != content {
			activity[session.Workspace] = now
		}
		prevCaps[session.Workspace] = previous
		captures[session.Workspace] = content
		sidebarStates[session.Workspace] = deriveSidebarAgentState(content, activity[session.Workspace], now)
		if tokens := parseAgentTokens(session.Workspace, content); tokens.Up != "" || tokens.Down != "" || tokens.Total != "" || tokens.Cost != "" {
			tokenData[session.Workspace] = tokens
		}
	}
}

func deriveSidebarAgentState(content string, lastActivity, now time.Time) string {
	if sidebarCaptureLooksIdle(content) {
		return sidebarAgentStateIdle
	}
	if sidebarCaptureLooksRunning(content) {
		return sidebarAgentStateRunning
	}
	if !lastActivity.IsZero() && !now.IsZero() && now.Sub(lastActivity) < sidebarRecentActivityWindow {
		return sidebarAgentStateRunning
	}
	return sidebarAgentStateIdle
}

func sidebarCaptureLooksRunning(content string) bool {
	lastLine := sidebarLastNonEmptyLine(content)
	if lastLine == "" || claudeDoneLineRe.MatchString(lastLine) {
		return false
	}
	return agentStateRe.MatchString(lastLine)
}

func sidebarLastNonEmptyLine(content string) string {
	lines := strings.Split(strings.TrimRight(sanitizeDisplayLine(content), "\n"), "\n")
	for i := len(lines) - 1; i >= 0; i-- {
		trimmed := strings.TrimSpace(lines[i])
		if trimmed != "" {
			return trimmed
		}
	}
	return ""
}

func sidebarCaptureLooksIdle(content string) bool {
	lastLine := sidebarLastNonEmptyLine(content)
	if lastLine == "" {
		return false
	}

	idlePatterns := []string{"❯", "> ", "$ ", "# ", "claude>"}
	for _, pattern := range idlePatterns {
		if strings.HasSuffix(lastLine, pattern) || lastLine == strings.TrimSpace(pattern) {
			return true
		}
	}
	return lastLine == ">" || lastLine == "❯"
}

func renderSidebarStateMarker(state string, tick int) string {
	switch state {
	case sidebarAgentStateRunning:
		frame := sidebarRunningFrames[(tick/6)%len(sidebarRunningFrames)]
		return lipgloss.NewStyle().Foreground(lipgloss.Color("6")).Render(frame)
	case sidebarAgentStateIdle:
		return lipgloss.NewStyle().Foreground(lipgloss.Color("2")).Render("●")
	default:
		return lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Render("○")
	}
}

func formatSidebarRowMeta(runtime, tokenSummary, attention string, width int) string {
	runtimeText := ""
	if runtime != "" {
		runtimeText = runtimeStyle.Render(runtime)
	}
	tokenText := ""
	if tokenSummary != "" {
		tokenText = tokenSidebarSty.Render(tokenSummary)
	}
	attentionText := ""
	if attention != "" {
		attentionText = taskFailClr.Render(attention)
	}

	return firstFittingDisplay(width,
		joinSidebarMeta(runtimeText, tokenText, attentionText),
		joinSidebarMeta(runtimeText, tokenText),
		joinSidebarMeta(runtimeText, attentionText),
		joinSidebarMeta(tokenText, attentionText),
		runtimeText,
		tokenText,
		attentionText,
	)
}

func joinSidebarMeta(parts ...string) string {
	filtered := make([]string, 0, len(parts))
	for _, part := range parts {
		part = strings.TrimSpace(part)
		if part != "" {
			filtered = append(filtered, part)
		}
	}
	return strings.Join(filtered, " ")
}

// tokenEntriesFromMap keeps only rows with parsed token data and sorts the
// detailed tokens pane by descending cost so the most expensive agents surface first.
func tokenEntriesFromMap(tokenData map[string]agentTokens) []agentTokens {
	entries := make([]agentTokens, 0, len(tokenData))
	for _, t := range tokenData {
		if t.Up != "" || t.Down != "" || t.Total != "" || t.Cost != "" {
			entries = append(entries, t)
		}
	}
	sort.Slice(entries, func(i, j int) bool {
		return parseCostValue(entries[i].Cost) > parseCostValue(entries[j].Cost)
	})
	return entries
}

// summarizeTokenEntries aggregates directional usage, standalone Claude done-line
// totals, and the subset of agents that reported an explicit cost.
func summarizeTokenEntries(entries []agentTokens, sessionCount int) tokenTotals {
	summary := tokenTotals{
		ReportingAgents: len(entries),
		SessionCount:    sessionCount,
	}
	for _, entry := range entries {
		summary.TotalUp += parseTokenValue(entry.Up)
		summary.TotalDown += parseTokenValue(entry.Down)
		summary.StandaloneTotal += parseTokenValue(entry.Total)
		summary.TotalCost += parseCostValue(entry.Cost)
		if entry.Cost != "" {
			summary.CostAgents++
		}
	}
	return summary
}

func formatTokenMetric(prefix string, raw string) string {
	if raw == "" {
		return ""
	}
	return prefix + formatTokenCount(parseTokenValue(raw))
}

func firstFittingDisplay(width int, candidates ...string) string {
	if width <= 0 {
		return ""
	}

	seen := make(map[string]struct{}, len(candidates))
	last := ""
	for _, candidate := range candidates {
		candidate = strings.TrimSpace(candidate)
		if candidate == "" {
			continue
		}
		if _, ok := seen[candidate]; ok {
			continue
		}
		seen[candidate] = struct{}{}
		if lipgloss.Width(candidate) <= width {
			return candidate
		}
		last = candidate
	}
	if last == "" {
		return ""
	}
	return fitDisplayText(last, width)
}

// formatSidebarTokenSummary compresses per-agent usage for narrow sidebar rows,
// preferring the richest representation that still fits.
func formatSidebarTokenSummary(tokens agentTokens, width int) string {
	if width <= 0 {
		return ""
	}

	up := formatTokenMetric("↑", tokens.Up)
	down := formatTokenMetric("↓", tokens.Down)
	total := formatTokenMetric("Σ", tokens.Total)
	io := strings.TrimSpace(strings.Join([]string{up, down}, " "))
	full := strings.TrimSpace(strings.Join([]string{up, down, total, tokens.Cost}, " "))

	return firstFittingDisplay(width, full, total, tokens.Cost, io)
}

// footerTokenSummary builds the always-visible aggregate usage line shown above
// the watch help text, including total-only Claude done-line usage when present.
func (m watchModel) footerTokenSummary(totalW int) string {
	summary := summarizeTokenEntries(tokenEntriesFromMap(m.tokenData), len(m.sessions))
	counts := fmt.Sprintf("%d/%d agents", summary.ReportingAgents, summary.SessionCount)
	if summary.ReportingAgents == 0 {
		return firstFittingDisplay(totalW,
			"usage "+counts+" · no token data",
			counts+" · no token data",
			fmt.Sprintf("%d agents · no token data", summary.SessionCount),
		)
	}

	io := strings.TrimSpace(strings.Join([]string{
		"↑" + formatTokenCount(summary.TotalUp),
		"↓" + formatTokenCount(summary.TotalDown),
	}, " "))
	total := ""
	if summary.StandaloneTotal > 0 {
		total = "Σ" + formatTokenCount(summary.StandaloneTotal)
	}
	cost := ""
	if summary.CostAgents > 0 {
		cost = fmt.Sprintf("$%.2f", summary.TotalCost)
	}
	return firstFittingDisplay(totalW,
		"usage "+counts+" · "+strings.TrimSpace(strings.Join([]string{io, total, cost}, " ")),
		counts+" · "+strings.TrimSpace(strings.Join([]string{io, total, cost}, " ")),
		counts+" · "+strings.TrimSpace(strings.Join([]string{total, cost}, " ")),
		counts+" · "+cost,
		counts+" · "+total,
		cost+" · "+counts,
	)
}

// extractDoneLineTotal parses Claude Code completion summaries such as
// "Done (16 tool uses · 93.9k tokens · 59s)" when no directional token split exists.
func extractDoneLineTotal(line string) string {
	if !claudeDoneLineRe.MatchString(line) {
		return ""
	}
	matches := tokenAnyRe.FindAllStringSubmatch(line, -1)
	if len(matches) == 0 {
		return ""
	}
	return matches[len(matches)-1][1]
}

// renderTaskListLines formats the compact two-line summary shown in the left
// column of the tasks split view.
func renderTaskListLines(task types.Task, selected bool, width int) []string {
	cursor := "  "
	nameStyle := lipgloss.NewStyle()
	if selected {
		cursor = selectedStyle.Render("▸ ")
		nameStyle = selectedStyle
	}

	status := taskStatusStyle(task.Status).Render(truncateStr(taskStatusLabel(task), 16))
	shortID := task.ID
	if len(shortID) > 8 {
		shortID = shortID[:8]
	}

	line1 := fmt.Sprintf("%s%s %s %s", cursor, msgTimeStyle.Render(shortID), status, nameStyle.Render(truncateStr(task.Title, max(8, width-32))))
	line1 = fitDisplayText(line1, width)
	meta := []string{taskPriorityLabel(task.Priority)}
	if attention := taskAttentionSummary(task); attention != "" {
		meta = append(meta, attention)
	}
	meta = append(meta,
		fmt.Sprintf("%s → %s", truncateStr(task.CreatedBy, 10), truncateStr(task.Assignee, 10)),
		formatTaskAge(task),
	)
	if last := taskLastUpdatePreview(task); last != "" {
		meta = append(meta, truncateStr(last, max(8, width-4)))
	}
	line2 := "   " + strings.Join(meta, " · ")
	return []string{line1, fitDisplayText(line2, width)}
}

// renderTaskSplitTopBorder draws the connected top border for the list/detail
// split layout used when at least one task matches the current filter.
func renderTaskSplitTopBorder(title string, listW, detailW int) string {
	leftSegmentW := listW + 1
	rightSegmentW := detailW + 1
	titleSegment := fitDisplayText(title, max(0, leftSegmentW-1))
	leftFill := leftSegmentW - 1 - lipgloss.Width(titleSegment)
	if leftFill < 0 {
		leftFill = 0
	}
	return taskBorderClr.Render("╭─") +
		titleSegment +
		taskBorderClr.Render(strings.Repeat("─", leftFill)+"┬"+strings.Repeat("─", rightSegmentW)+"╮")
}

// renderTaskSplitBottomBorder mirrors renderTaskSplitTopBorder so the divider
// column stays aligned across the full split view.
func renderTaskSplitBottomBorder(listW, detailW int) string {
	leftSegmentW := listW + 1
	rightSegmentW := detailW + 1
	return taskBorderClr.Render("╰" + strings.Repeat("─", leftSegmentW) + "┴" + strings.Repeat("─", rightSegmentW) + "╯")
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

	var up, down, total, cost, status string
	for _, rawLine := range lines[start:] {
		line := sanitizeDisplayLine(rawLine)
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
	if up == "" && down == "" {
		for i := len(lines) - 1; i >= start; i-- {
			if parsed := extractDoneLineTotal(sanitizeDisplayLine(lines[i])); parsed != "" {
				total = "Σ" + parsed
				break
			}
		}
	}

	var parts []string
	if up != "" || down != "" {
		parts = append(parts, strings.TrimSpace(up+" "+down))
	} else if total != "" {
		parts = append(parts, total)
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

func parseAgentTokens(workspace, content string) agentTokens {
	lines := strings.Split(content, "\n")
	start := len(lines) - 30
	if start < 0 {
		start = 0
	}

	var t agentTokens
	t.Workspace = workspace
	for _, rawLine := range lines[start:] {
		line := sanitizeDisplayLine(rawLine)
		if m := tokenUpRe.FindStringSubmatch(line); m != nil {
			t.Up = m[1]
		}
		if m := tokenDownRe.FindStringSubmatch(line); m != nil {
			t.Down = m[1]
		}
		if m := costRe.FindString(line); m != "" {
			t.Cost = m
		}
	}
	if t.Up == "" && t.Down == "" {
		for i := len(lines) - 1; i >= start; i-- {
			if parsed := extractDoneLineTotal(sanitizeDisplayLine(lines[i])); parsed != "" {
				t.Total = parsed
				break
			}
		}
	}
	return t
}

func parseTokenValue(s string) float64 {
	if s == "" {
		return 0
	}
	s = strings.TrimSpace(s)
	multiplier := 1.0
	if strings.HasSuffix(s, "k") || strings.HasSuffix(s, "K") {
		multiplier = 1000
		s = s[:len(s)-1]
	} else if strings.HasSuffix(s, "m") || strings.HasSuffix(s, "M") {
		multiplier = 1000000
		s = s[:len(s)-1]
	}
	var val float64
	fmt.Sscanf(s, "%f", &val)
	return val * multiplier
}

func parseCostValue(s string) float64 {
	if s == "" {
		return 0
	}
	s = strings.TrimPrefix(s, "$")
	var val float64
	fmt.Sscanf(s, "%f", &val)
	return val
}

func formatTokenCount(v float64) string {
	if v >= 1000000 {
		return fmt.Sprintf("%.1fM", v/1000000)
	}
	if v >= 1000 {
		return fmt.Sprintf("%.1fk", v/1000)
	}
	return fmt.Sprintf("%.0f", v)
}

func liveTokenLines(innerW int, entries []agentTokens, summary tokenTotals) []string {
	lines := []string{tokenDimClr.Render(" live usage ")}
	if len(entries) == 0 {
		return append(lines, tokenDimClr.Render("  (no token data)"))
	}

	header := fmt.Sprintf(" %-24s %10s %10s %10s", "WORKSPACE", "INPUT", "OUTPUT", "COST")
	lines = append(lines, tokenDimClr.Render(header))

	maxCost := 0.0
	for _, e := range entries {
		if c := parseCostValue(e.Cost); c > maxCost {
			maxCost = c
		}
	}

	for _, e := range entries {
		up := parseTokenValue(e.Up)
		down := parseTokenValue(e.Down)
		total := parseTokenValue(e.Total)
		cost := parseCostValue(e.Cost)

		sty := tokenNormalClr
		if maxCost > 0 && cost >= maxCost*0.8 {
			sty = tokenHighClr
		}

		upStr := "-"
		if e.Up != "" {
			upStr = "↑" + formatTokenCount(up)
		}
		downStr := "-"
		if e.Down != "" {
			downStr = "↓" + formatTokenCount(down)
		} else if e.Total != "" {
			downStr = "Σ" + formatTokenCount(total)
		}
		costStr := e.Cost
		if costStr == "" {
			costStr = "-"
		}

		ws := e.Workspace
		if len(ws) > 24 {
			ws = ws[:23] + "…"
		}
		line := fmt.Sprintf(" %-24s %10s %10s %10s", ws, upStr, downStr, costStr)
		lines = append(lines, sty.Render(line))
	}

	lines = append(lines, tokenDimClr.Render(" "+strings.Repeat("─", max(0, innerW-2))))
	totalOut := "-"
	if summary.TotalDown > 0 {
		totalOut = "↓" + formatTokenCount(summary.TotalDown)
	} else if summary.StandaloneTotal > 0 {
		totalOut = "Σ" + formatTokenCount(summary.StandaloneTotal)
	}
	totalIn := "-"
	if summary.TotalUp > 0 {
		totalIn = "↑" + formatTokenCount(summary.TotalUp)
	}
	totalLine := fmt.Sprintf(" %-24s %10s %10s %10s",
		fmt.Sprintf("TOTAL (%d agents)", summary.ReportingAgents),
		totalIn,
		totalOut,
		func() string {
			if summary.CostAgents == 0 {
				return "-"
			}
			return fmt.Sprintf("$%.2f", summary.TotalCost)
		}())
	lines = append(lines, tokenSumClr.Render(totalLine))
	return lines
}

func trendTokenLines(innerW int, sessions []tmux.SessionInfo, trendData map[string]usage.WorkspaceTrend) []string {
	lines := []string{tokenDimClr.Render(" history 24h ")}
	seen := make(map[string]struct{}, len(sessions))
	workspaceOrder := make([]string, 0, len(sessions))
	for _, session := range sessions {
		if _, ok := seen[session.Workspace]; ok {
			continue
		}
		seen[session.Workspace] = struct{}{}
		workspaceOrder = append(workspaceOrder, session.Workspace)
	}

	type trendRow struct {
		workspace string
		last      int64
		total     int64
		spark     string
	}
	rows := make([]trendRow, 0, len(workspaceOrder))
	unavailable := 0
	for _, workspace := range workspaceOrder {
		trend, ok := trendData[workspace]
		if !ok || !trend.Available {
			unavailable++
			continue
		}
		bucketTotals := make([]int64, 0, len(trend.Buckets))
		for _, bucket := range trend.Buckets {
			bucketTotals = append(bucketTotals, bucket.Totals.Total())
		}
		rows = append(rows, trendRow{
			workspace: workspace,
			last:      trend.LatestTokens.Total(),
			total:     trend.Total.Total(),
			spark:     renderTokenSparkline(bucketTotals),
		})
	}

	sort.Slice(rows, func(i, j int) bool {
		if rows[i].total == rows[j].total {
			return rows[i].workspace < rows[j].workspace
		}
		return rows[i].total > rows[j].total
	})

	if len(rows) == 0 {
		if unavailable > 0 {
			return append(lines, tokenDimClr.Render(fmt.Sprintf("  (%d workspace(s) unavailable)", unavailable)))
		}
		return append(lines, tokenDimClr.Render("  (no history data yet)"))
	}

	header := fmt.Sprintf(" %-18s %10s %10s %-8s", "WORKSPACE", "LAST", "24H", "TREND")
	lines = append(lines, tokenDimClr.Render(header))
	for _, row := range rows {
		ws := row.workspace
		if len(ws) > 18 {
			ws = ws[:17] + "…"
		}
		line := fmt.Sprintf(" %-18s %10s %10s %-8s",
			ws,
			formatTokenCount(float64(row.last)),
			formatTokenCount(float64(row.total)),
			row.spark,
		)
		lines = append(lines, tokenNormalClr.Render(line))
	}
	if unavailable > 0 {
		lines = append(lines, tokenDimClr.Render(fmt.Sprintf("  %d workspace(s) unavailable", unavailable)))
	}
	return lines
}

func renderTokenSparkline(values []int64) string {
	if len(values) == 0 {
		return ""
	}
	var maxValue int64
	for _, value := range values {
		if value > maxValue {
			maxValue = value
		}
	}
	if maxValue <= 0 {
		return strings.Repeat("·", len(values))
	}

	levels := []rune("▁▂▃▄▅▆▇█")
	out := make([]rune, len(values))
	for i, value := range values {
		if value <= 0 {
			out[i] = '·'
			continue
		}
		idx := int(float64(value) / float64(maxValue) * float64(len(levels)-1))
		if idx < 0 {
			idx = 0
		}
		if idx >= len(levels) {
			idx = len(levels) - 1
		}
		out[i] = levels[idx]
	}
	return string(out)
}

func splitTokenSectionBudget(innerH int, wantTrend bool) (int, int) {
	if !wantTrend || innerH < 6 {
		return innerH, 0
	}
	trendH := innerH / 3
	if trendH < 2 {
		trendH = 2
	}
	liveH := innerH - trendH
	if liveH < 4 {
		liveH = min(4, innerH)
		trendH = innerH - liveH
	}
	if trendH < 0 {
		trendH = 0
	}
	return liveH, trendH
}

var (
	tokenBorderClr = lipgloss.NewStyle().Foreground(lipgloss.Color("13"))
	tokenTitleSty  = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("13"))
	tokenHighClr   = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("9"))
	tokenNormalClr = lipgloss.NewStyle().Foreground(lipgloss.Color("7"))
	tokenDimClr    = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	tokenSumClr    = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("6"))
)

// renderTokens renders the optional detailed usage pane. Rows that only have a
// Claude done-line total use the output column to show Σ total tokens.
func (m watchModel) renderTokens(totalW, totalH int) string {
	innerW := totalW - 2
	innerH := totalH - 2
	if innerH < 1 {
		innerH = 1
	}

	title := tokenTitleSty.Render(" tokens ")
	titleW := lipgloss.Width(title)
	pad := innerW - titleW - 1
	if pad < 0 {
		pad = 0
	}
	topLine := tokenBorderClr.Render("╭─") + title + tokenBorderClr.Render(strings.Repeat("─", pad)+"╮")

	entries := tokenEntriesFromMap(m.tokenData)
	summary := summarizeTokenEntries(entries, len(m.sessions))
	liveLines := liveTokenLines(innerW, entries, summary)
	trendLines := trendTokenLines(innerW, m.sessions, m.trendData)
	liveBudget, trendBudget := splitTokenSectionBudget(innerH, len(trendLines) > 0)

	var tokenLines []string
	tokenLines = append(tokenLines, liveLines[:min(len(liveLines), liveBudget)]...)
	if trendBudget > 0 {
		tokenLines = append(tokenLines, trendLines[:min(len(trendLines), trendBudget)]...)
	}

	var bodyLines []string
	for i := 0; i < innerH; i++ {
		line := ""
		if i < len(tokenLines) {
			line = tokenLines[i]
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
		bodyLines = append(bodyLines, tokenBorderClr.Render("│")+line+strings.Repeat(" ", padding)+tokenBorderClr.Render("│"))
	}

	botLine := tokenBorderClr.Render("╰" + strings.Repeat("─", innerW) + "╯")

	all := []string{topLine}
	all = append(all, bodyLines...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

func sanitizeDisplayLine(s string) string {
	s = xansi.Strip(s)
	s = strings.ReplaceAll(s, "\t", "    ")
	return strings.Map(func(r rune) rune {
		switch {
		case r == '\n':
			return r
		case unicode.IsControl(r):
			return -1
		default:
			return r
		}
	}, s)
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

func readHistoryFileIfChanged(path string, lastMod time.Time, cached []daemon.HistoryEntry, maxEntries int) ([]daemon.HistoryEntry, time.Time) {
	info, err := os.Stat(path)
	if err != nil {
		return nil, time.Time{}
	}
	modTime := info.ModTime()
	if !lastMod.IsZero() && !modTime.After(lastMod) {
		return cached, lastMod
	}
	return readHistoryFile(path, maxEntries), modTime
}

func readTasksFile(path string) []types.Task {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var tasks []types.Task
	if json.Unmarshal(data, &tasks) != nil {
		return nil
	}
	// Sort: in_progress first, then pending, then completed/failed
	sort.Slice(tasks, func(i, j int) bool {
		oi := taskSortOrder(tasks[i].Status)
		oj := taskSortOrder(tasks[j].Status)
		if oi != oj {
			return oi < oj
		}
		pi := taskPriorityOrder(tasks[i].Priority)
		pj := taskPriorityOrder(tasks[j].Priority)
		if pi != pj {
			return pi < pj
		}
		return tasks[i].UpdatedAt.After(tasks[j].UpdatedAt)
	})
	return tasks
}

func readTasksFileIfChanged(path string, lastMod time.Time, cached []types.Task) ([]types.Task, time.Time) {
	info, err := os.Stat(path)
	if err != nil {
		return nil, time.Time{}
	}
	modTime := info.ModTime()
	if !lastMod.IsZero() && !modTime.After(lastMod) {
		return cached, lastMod
	}
	return readTasksFile(path), modTime
}

func taskSortOrder(s types.TaskStatus) int {
	switch s {
	case types.TaskInProgress:
		return 0
	case types.TaskPending:
		return 1
	case types.TaskFailed:
		return 2
	case types.TaskCompleted:
		return 3
	default:
		return 4
	}
}

func taskStatusStyle(s types.TaskStatus) lipgloss.Style {
	switch s {
	case types.TaskPending:
		return taskPendingClr
	case types.TaskInProgress:
		return taskActiveClr
	case types.TaskCompleted:
		return taskDoneClr
	case types.TaskFailed:
		return taskFailClr
	default:
		return taskPendingClr
	}
}

// renderTasks renders the filtered task list and the currently selected task's
// detail pane in a connected split layout.
func (m watchModel) renderTasks(totalW, totalH int) string {
	innerW := totalW - 2
	innerH := totalH - 2
	if innerH < 1 {
		innerH = 1
	}

	filtered := filterTasks(m.tasks, m.taskFilter)
	title := taskTitleStyle.Render(fmt.Sprintf(" tasks %s %d/%d ", m.taskFilter.label(), len(filtered), len(m.tasks)))
	titleW := lipgloss.Width(title)

	var bodyLines []string
	var topLine string
	var botLine string
	if len(filtered) == 0 {
		pad := innerW - titleW - 1
		if pad < 0 {
			pad = 0
		}
		topLine = taskBorderClr.Render("╭─") + title + taskBorderClr.Render(strings.Repeat("─", pad)+"╮")
		for i := 0; i < innerH; i++ {
			line := ""
			if i == 0 {
				line = taskPendingClr.Render(" no tasks for current filter ")
			}
			padding := innerW - lipgloss.Width(line)
			if padding < 0 {
				padding = 0
			}
			bodyLines = append(bodyLines, taskBorderClr.Render("│")+line+strings.Repeat(" ", padding)+taskBorderClr.Render("│"))
		}
		botLine = taskBorderClr.Render("╰" + strings.Repeat("─", innerW) + "╯")
	} else {
		listW := innerW * 42 / 100
		if listW < 44 {
			listW = 44
		}
		if listW > innerW-28 {
			listW = innerW - 28
		}
		detailW := innerW - listW - 3
		if detailW < 24 {
			detailW = 24
			listW = innerW - detailW - 3
		}
		topLine = renderTaskSplitTopBorder(title, listW, detailW)
		botLine = renderTaskSplitBottomBorder(listW, detailW)

		selectedIdx := clampTaskSelection(m.taskSelected, m.tasks, m.taskFilter)
		viewport := computeTaskListViewport(len(filtered), selectedIdx, innerH)
		var listLines []string
		for i := viewport.Start; i < viewport.End; i++ {
			listLines = append(listLines, renderTaskListLines(filtered[i], i == selectedIdx, listW)...)
		}

		task := filtered[selectedIdx]
		detailLines := renderTaskDetailLines(task, m.msgHistory, detailW, innerH)

		for i := 0; i < innerH; i++ {
			left := ""
			if i < len(listLines) {
				left = fitDisplayText(listLines[i], listW)
			}
			leftPad := listW - lipgloss.Width(left)
			if leftPad < 0 {
				leftPad = 0
			}

			right := ""
			if i < len(detailLines) {
				right = fitDisplayText(detailLines[i], detailW)
			}
			rightPad := detailW - lipgloss.Width(right)
			if rightPad < 0 {
				rightPad = 0
			}

			line := left + strings.Repeat(" ", leftPad) + taskBorderClr.Render(" │ ") + right + strings.Repeat(" ", rightPad)
			bodyLines = append(bodyLines, taskBorderClr.Render("│")+line+taskBorderClr.Render("│"))
		}
	}

	all := []string{topLine}
	all = append(all, bodyLines...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

func renderTaskDetailLines(task types.Task, history []daemon.HistoryEntry, width, height int) []string {
	var lines []string
	stale := "no"
	if taskIsStale(task) {
		stale = "yes"
	}
	lines = append(lines,
		headerStyle.Render(truncateStr(task.Title, width)),
		fmt.Sprintf("status: %s", taskStatusLabel(task)),
		fmt.Sprintf("assignee: %s", task.Assignee),
		fmt.Sprintf("created_by: %s", task.CreatedBy),
		fmt.Sprintf("priority: %s", taskPriorityLabel(task.Priority)),
		fmt.Sprintf("updated: %s ago", formatTaskAge(task)),
		fmt.Sprintf("start_mode: %s", task.StartMode),
		fmt.Sprintf("stale: %s", stale),
	)
	if task.StaleAfterSeconds > 0 {
		lines = append(lines, fmt.Sprintf("stale_after: %ds", task.StaleAfterSeconds))
	}
	if task.Description != "" {
		lines = append(lines, "", "desc: "+truncateStr(task.Description, width))
	}
	if task.Result != "" {
		lines = append(lines, "", "result: "+truncateStr(task.Result, width))
	}
	if task.StaleInfo != nil {
		lines = append(lines, "", "stale_info:")
		lines = append(lines, "  reason: "+truncateStr(task.StaleInfo.Reason, max(0, width-10)))
		if task.StaleInfo.RecommendedAction != "" {
			lines = append(lines, "  action: "+truncateStr(task.StaleInfo.RecommendedAction, max(0, width-10)))
		}
		if task.StaleInfo.PendingMessages > 0 {
			lines = append(lines, fmt.Sprintf("  pending_messages: %d", task.StaleInfo.PendingMessages))
		}
		if task.StaleInfo.StateDivergence {
			lines = append(lines, "  divergence: "+truncateStr(task.StaleInfo.StateDivergenceNote, max(0, width-14)))
		}
	}

	logs := recentTaskLogs(task, 3)
	if len(logs) > 0 {
		lines = append(lines, "", "recent logs:")
		for _, log := range logs {
			lines = append(lines, truncateStr(fmt.Sprintf("  %s %s: %s", log.Timestamp.Format("15:04:05"), log.Workspace, log.Message), width))
		}
	}

	activity := buildTaskActivity(task, history, 4)
	if len(activity) > 0 {
		lines = append(lines, "", "activity:")
		for _, entry := range activity {
			lines = append(lines, truncateStr(fmt.Sprintf("  %s %-9s %s", entry.Timestamp.Format("15:04:05"), activityKindLabel(entry.Kind), entry.Summary), width))
		}
	}

	if len(lines) > height {
		lines = lines[:height]
	}
	return lines
}

func max(a, b int) int {
	if a > b {
		return a
	}
	return b
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

func fitDisplayText(s string, width int) string {
	if width <= 0 {
		return ""
	}
	return xansi.Truncate(s, width, "…")
}

func init() {
	watchCmd.Flags().BoolVar(&watchShowAgents, "agents", false, "open the agents-only watch view (stream pane hidden)")
	watchCmd.Flags().BoolVar(&watchShowTasks, "tasks", false, "open the tasks watch view")
	watchCmd.Flags().BoolVar(&watchShowMessages, "messages", false, "open the messages watch view")
	watchCmd.Flags().BoolVar(&watchShowTokens, "tokens", false, "open the tokens watch view")
	rootCmd.AddCommand(watchCmd)
}
