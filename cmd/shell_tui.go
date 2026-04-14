package cmd

import (
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/usage"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type inputMode int

const (
	modeInput inputMode = iota
	modeControl
)

var modeInputStyle = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("2"))
var modeControlStyle = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("3"))

type shellModel struct {
	width                  int
	height                 int
	spinnerTick            int
	dataRefreshedAt        time.Time
	sessionsRefreshedAt    time.Time
	forceDataRefresh       bool
	selected               int
	captureCursor          int
	captures               map[string]string
	prevCaps               map[string]string
	activity               map[string]time.Time
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
	taskSelected           int
	taskFilter             taskFilterMode
	mainResize             tmuxResizeState
	previewResize          tmuxResizeState

	mode        inputMode
	viewTarget  string // workspace shown in main pane
	orchSession string // tmux session name for orchestrator
}

func newShellModel(orchSession, socketPath string) shellModel {
	return shellModel{
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
		stream:           streamMessages,
		taskFilter:       taskFilterActive,
		mode:             modeInput,
		viewTarget:       "orchestrator",
		orchSession:      orchSession,
		forceDataRefresh: true,
	}
}

func (m shellModel) Init() tea.Cmd {
	return tea.Batch(tickCmd(), tea.WindowSize())
}

func (m shellModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch m.mode {
		case modeInput:
			return m.handleInputMode(msg)
		case modeControl:
			return m.handleControlMode(msg)
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

		// Resize main pane (viewTarget) and preview pane (selected) to match
		if m.width > 0 {
			mainW, mainH, previewH := m.layoutHeights()
			if mainW > 10 && mainH > 5 {
				if viewSession := m.currentViewSession(); viewSession != "" {
					resizeTmuxWindowIfNeeded(&m.mainResize, viewSession, mainW, mainH)
				}
			}
			if previewH > 5 {
				if previewSession := m.previewSession(); previewSession != "" {
					resizeTmuxWindowIfNeeded(&m.previewResize, previewSession, mainW, previewH)
				}
			}
		}

		targets, nextCursor := planCaptureTargets(m.sessions, m.focusedWorkspaces(), m.captureCursor, watchBackgroundCaptureBatchSize)
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

func (m shellModel) handleInputMode(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	if msg.String() == "ctrl+a" {
		m.mode = modeControl
		return m, nil
	}
	return m, m.forwardKey(msg)
}

func (m shellModel) handleControlMode(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.String() {
	// Navigation — stay in control mode
	case "k", "up":
		m.selected = moveSelection(m.selected, m.sessions, -1)
		m.forceDataRefresh = true
	case "j", "down":
		m.selected = moveSelection(m.selected, m.sessions, 1)
		m.forceDataRefresh = true

	// Actions — execute and return to input mode
	case "q":
		return m, tea.Quit
	case "t":
		m.stream = (m.stream + 1) % streamViewCount
		m.mode = modeInput
	case "[":
		m.taskSelected = moveTaskSelection(m.taskSelected, m.tasks, m.taskFilter, -1)
		m.mode = modeInput
	case "]":
		m.taskSelected = moveTaskSelection(m.taskSelected, m.tasks, m.taskFilter, 1)
		m.mode = modeInput
	case "f":
		m.taskFilter = nextTaskFilterMode(m.taskFilter)
		m.taskSelected = clampTaskSelection(m.taskSelected, m.tasks, m.taskFilter)
		m.mode = modeInput
	case "x":
		if m.selected < len(m.sessions) {
			_ = tmux.InterruptWorkspace(m.sessions[m.selected].Workspace)
		}
		m.mode = modeInput
	case "v":
		if m.selected < len(m.sessions) {
			m.viewTarget = m.sessions[m.selected].Workspace
		}
		m.forceDataRefresh = true
		m.mode = modeInput
	case "o":
		m.viewTarget = "orchestrator"
		m.forceDataRefresh = true
		m.mode = modeInput
	case "ctrl+a":
		m.mode = modeInput
		session := m.currentViewSession()
		if session != "" {
			return m, func() tea.Msg {
				tmux.SendSpecialKeyToSession(session, "C-a")
				return nil
			}
		}

	// Escape or any other key — return to input mode
	default:
		m.mode = modeInput
	}
	return m, nil
}

func (m shellModel) forwardKey(msg tea.KeyMsg) tea.Cmd {
	session := m.currentViewSession()
	if session == "" {
		return nil
	}

	return func() tea.Msg {
		key := msg.String()
		switch key {
		case "enter":
			tmux.SendSpecialKeyToSession(session, "Enter")
		case "tab":
			tmux.SendSpecialKeyToSession(session, "Tab")
		case "backspace":
			tmux.SendSpecialKeyToSession(session, "BSpace")
		case "delete":
			tmux.SendSpecialKeyToSession(session, "DC")
		case "escape", "esc":
			tmux.SendSpecialKeyToSession(session, "Escape")
		case "up":
			tmux.SendSpecialKeyToSession(session, "Up")
		case "down":
			tmux.SendSpecialKeyToSession(session, "Down")
		case "left":
			tmux.SendSpecialKeyToSession(session, "Left")
		case "right":
			tmux.SendSpecialKeyToSession(session, "Right")
		case "home":
			tmux.SendSpecialKeyToSession(session, "Home")
		case "end":
			tmux.SendSpecialKeyToSession(session, "End")
		case "pgup":
			tmux.SendSpecialKeyToSession(session, "PPage")
		case "pgdown":
			tmux.SendSpecialKeyToSession(session, "NPage")
		case "ctrl+c":
			tmux.SendSpecialKeyToSession(session, "C-c")
		case "ctrl+d":
			tmux.SendSpecialKeyToSession(session, "C-d")
		case "ctrl+u":
			tmux.SendSpecialKeyToSession(session, "C-u")
		case "ctrl+l":
			tmux.SendSpecialKeyToSession(session, "C-l")
		case "ctrl+z":
			tmux.SendSpecialKeyToSession(session, "C-z")
		case "ctrl+r":
			tmux.SendSpecialKeyToSession(session, "C-r")
		case "ctrl+w":
			tmux.SendSpecialKeyToSession(session, "C-w")
		case "ctrl+e":
			tmux.SendSpecialKeyToSession(session, "C-e")
		case "ctrl+k":
			tmux.SendSpecialKeyToSession(session, "C-k")
		case "ctrl+b":
			tmux.SendSpecialKeyToSession(session, "C-b")
		case "ctrl+f":
			tmux.SendSpecialKeyToSession(session, "C-f")
		case "ctrl+p":
			tmux.SendSpecialKeyToSession(session, "C-p")
		case "ctrl+n":
			tmux.SendSpecialKeyToSession(session, "C-n")
		case " ":
			tmux.SendRawKey(session, " ")
		default:
			if len(msg.Runes) > 0 {
				tmux.SendRawKey(session, string(msg.Runes))
			}
		}
		return nil
	}
}

func (m shellModel) currentViewSession() string {
	for _, s := range m.sessions {
		if s.Workspace == m.viewTarget {
			return s.Name
		}
	}
	return ""
}

// previewWorkspace returns the workspace name that should be shown in the
// preview pane, or empty string if no preview should be shown.
func (m shellModel) previewWorkspace() string {
	if m.selected >= len(m.sessions) {
		return ""
	}
	ws := m.sessions[m.selected].Workspace
	if ws == m.viewTarget {
		return ""
	}
	return ws
}

func (m shellModel) previewSession() string {
	ws := m.previewWorkspace()
	if ws == "" {
		return ""
	}
	for _, s := range m.sessions {
		if s.Workspace == ws {
			return s.Name
		}
	}
	return ""
}

func (m shellModel) focusedWorkspaces() map[string]bool {
	focused := make(map[string]bool, 2)
	if m.viewTarget != "" {
		focused[m.viewTarget] = true
	}
	if preview := m.previewWorkspace(); preview != "" {
		focused[preview] = true
	}
	return focused
}

// layoutHeights computes the inner dimensions for the main pane and preview
// pane based on the current terminal size. Returns (mainW, mainH, previewH).
// previewH is 0 when no preview pane is shown.
func (m shellModel) layoutHeights() (int, int, int) {
	sideW := watchSidebarWidth
	mainW := m.width - sideW - 2 // inner content width
	streamH := streamPaneHeight(m.height, m.stream)
	totalInner := m.height - streamH - 3

	if m.previewWorkspace() == "" {
		return mainW, totalInner, 0
	}

	// Split: main = 60%, preview = remaining. Each pane has 2 rows of border.
	// totalInner is the sum of both panes' inner heights + border overhead.
	// We allocate outer heights, then subtract borders for inner.
	outerTotal := totalInner + 2 // total outer rows available for the two stacked panes + their borders
	mainOuter := (outerTotal * 6) / 10
	if mainOuter < 7 {
		mainOuter = 7
	}
	previewOuter := outerTotal - mainOuter
	if previewOuter < 5 {
		previewOuter = 5
		mainOuter = outerTotal - previewOuter
	}
	mainH := mainOuter - 2
	previewH := previewOuter - 2
	if mainH < 3 {
		mainH = 3
	}
	if previewH < 3 {
		previewH = 3
	}
	return mainW, mainH, previewH
}

func (m shellModel) View() string {
	if m.width == 0 || len(m.sessions) == 0 {
		return "Loading... (waiting for sessions)"
	}

	sideW := watchSidebarWidth
	mainW := m.width - sideW
	if mainW < 20 {
		mainW = 20
	}

	streamH := streamPaneHeight(m.height, m.stream)
	contentH := m.height - streamH - 1

	sidebar := m.renderSidebar(sideW, contentH)

	// Right column: main pane (+ optional preview pane below)
	var rightCol string
	previewWs := m.previewWorkspace()
	if previewWs == "" {
		mainContent := m.renderMain(m.viewTarget, m.captures[m.viewTarget], mainW, contentH)
		rightCol = mainContent
	} else {
		_, mainInnerH, previewInnerH := m.layoutHeights()
		mainOuterH := mainInnerH + 2
		previewOuterH := previewInnerH + 2

		mainContent := m.renderMain(m.viewTarget, m.captures[m.viewTarget], mainW, mainOuterH)
		previewContent := m.renderMain(previewWs+" (preview)", m.captures[previewWs], mainW, previewOuterH)
		rightCol = lipgloss.JoinVertical(lipgloss.Left, mainContent, previewContent)
	}

	top := lipgloss.JoinHorizontal(lipgloss.Top, sidebar, rightCol)

	var stream string
	switch m.stream {
	case streamMessages:
		stream = m.renderStream(m.width, streamH)
	case streamTasks:
		stream = m.renderTasks(m.width, streamH)
	case streamTokens:
		stream = m.renderTokens(m.width, streamH)
	}

	help := m.renderHelp()

	parts := []string{top}
	if stream != "" {
		parts = append(parts, stream)
	}
	parts = append(parts, help)

	return lipgloss.JoinVertical(lipgloss.Left, parts...)
}

func (m shellModel) renderHelp() string {
	if m.mode == modeControl {
		return modeControlStyle.Render(" [CTRL] ") +
			lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Render(
				"j/k select · v view · o orch · t msgs/tasks/tokens/off · [/ ] task · f filter · x interrupt · q quit · esc back")
	}
	return modeInputStyle.Render(" [INPUT] ") +
		lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Render(
			"Ctrl+A: control mode")
}

// Delegate rendering to watch.go's functions via wrapper methods

func (m shellModel) renderSidebar(w, h int) string {
	wm := watchModel{
		width:           m.width,
		height:          m.height,
		spinnerTick:     m.spinnerTick,
		dataRefreshedAt: m.dataRefreshedAt,
		selected:        m.selected,
		captures:        m.captures,
		prevCaps:        m.prevCaps,
		activity:        m.activity,
		sessions:        m.sessions,
		runtimes:        m.runtimes,
		tasks:           m.tasks,
		tokenData:       m.tokenData,
		sidebarStates:   m.sidebarStates,
		workspaceInfos:  m.workspaceInfos,
	}
	return wm.renderSidebar(w, h)
}

func (m shellModel) renderMain(ws, content string, w, h int) string {
	wm := watchModel{}
	return wm.renderMain(ws, content, w, h)
}

func (m shellModel) renderStream(totalW, totalH int) string {
	wm := watchModel{
		msgHistory: m.msgHistory,
	}
	return wm.renderStream(totalW, totalH)
}

func (m shellModel) renderTasks(totalW, totalH int) string {
	wm := watchModel{
		tasks:        m.tasks,
		msgHistory:   m.msgHistory,
		taskSelected: m.taskSelected,
		taskFilter:   m.taskFilter,
	}
	return wm.renderTasks(totalW, totalH)
}

func (m shellModel) renderTokens(totalW, totalH int) string {
	wm := watchModel{
		sessions:  m.sessions,
		tokenData: m.tokenData,
		trendData: m.trendData,
	}
	return wm.renderTokens(totalW, totalH)
}
