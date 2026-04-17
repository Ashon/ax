package cmd

import (
	"fmt"
	"os/exec"
	"regexp"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/usage"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/spf13/cobra"
)

var watchCmd = &cobra.Command{
	Use:     "top",
	Aliases: []string{"watch"},
	Short:   "Monitor workspace sessions with top-style TUI",
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

const watchFPS = 12
const watchMessagePaneMinHeight = 6
const watchSidebarWidth = 34
const watchDataRefreshInterval = 250 * time.Millisecond
const watchSessionRefreshInterval = time.Second
const watchBackgroundCaptureBatchSize = 2
const watchWorkspaceRefreshInterval = time.Second
const watchTrendRefreshInterval = 15 * time.Second
const watchTrendWindowMinutes = 24 * 60
const watchTrendBucketMinutes = 3 * 60
const watchBackgroundCaptureMaxAge = 2 * time.Second

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
	quickActions           []watchQuickAction
	quickActionsOpen       bool
	quickActionSelected    int
	quickActionConfirm     bool
	noticeText             string
	noticeErr              bool
	noticeUntil            time.Time
	stream                 streamView
	streamOnly             bool
	mainResize             tmuxResizeState
}

type sidebarEntry struct {
	label        string
	workspace    string
	reconcile    string
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
		return streamMessages, false, fmt.Errorf("top view flags are mutually exclusive; use only one of --agents, --tasks, --messages, or --tokens")
	}
	return selected, streamOnly, nil
}

func (m watchModel) Init() tea.Cmd {
	return tea.Batch(tickCmd(), tea.WindowSize())
}

func (m watchModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		if msg.String() == "q" || msg.String() == "ctrl+c" {
			return m, tea.Quit
		}
		if m.quickActionsOpen {
			switch msg.String() {
			case "esc":
				m.closeQuickActions()
			case "up", "k":
				m.moveQuickActionSelection(-1)
			case "down", "j":
				m.moveQuickActionSelection(1)
			case "enter":
				m.runSelectedQuickAction()
			}
			return m, nil
		}
		switch msg.String() {
		case "up", "k":
			m.selected = moveSelection(m.selected, m.sessions, -1)
			m.forceDataRefresh = true
		case "down", "j":
			m.selected = moveSelection(m.selected, m.sessions, 1)
			m.forceDataRefresh = true
		case "enter":
			if !m.streamOnly {
				m.openQuickActions()
			}
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
		if m.quickActionsOpen && m.selectedWorkspaceName() == "" {
			m.closeQuickActions()
		}
		m.clearExpiredNotice(now)

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

		focused := watchFocusedWorkspaces(m.sessions, m.selected)
		targets, nextCursor := planCaptureTargets(m.sessions, focused, m.captureCursor, watchBackgroundCaptureBatchSize)
		m.captureCursor = nextCursor
		refreshSessionCaptures(targets, focused, m.captures, m.prevCaps, m.activity, m.sidebarStates, m.tokenData, now)

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

func refreshSessionCaptures(targets []tmux.SessionInfo, focused map[string]bool, captures, prevCaps map[string]string, activity map[string]time.Time, sidebarStates map[string]string, tokenData map[string]agentTokens, now time.Time) {
	for _, session := range targets {
		maxAge := watchBackgroundCaptureMaxAge
		if focused[session.Workspace] {
			maxAge = 0
		}
		content := capturePaneThrottled(session.Name, maxAge, now)
		previous := captures[session.Workspace]
		if previous != "" && previous != content {
			activity[session.Workspace] = now
		}
		prevCaps[session.Workspace] = previous
		captures[session.Workspace] = content
		sidebarStates[session.Workspace] = deriveSidebarAgentState(content, activity[session.Workspace], now)
		if previous == content {
			continue
		}
		if tokens := parseAgentTokens(session.Workspace, content); tokens.Up != "" || tokens.Down != "" || tokens.Total != "" || tokens.Cost != "" {
			tokenData[session.Workspace] = tokens
		} else {
			delete(tokenData, session.Workspace)
		}
	}
}

func orderedLeafSessionIndices(sessions []tmux.SessionInfo) []int {
	entries := buildSidebarEntriesCached(sessions)
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

func init() {
	watchCmd.Flags().BoolVar(&watchShowAgents, "agents", false, "open the agents-only top view (stream pane hidden)")
	watchCmd.Flags().BoolVar(&watchShowTasks, "tasks", false, "open the tasks top view")
	watchCmd.Flags().BoolVar(&watchShowMessages, "messages", false, "open the messages top view")
	watchCmd.Flags().BoolVar(&watchShowTokens, "tokens", false, "open the tokens top view")
	rootCmd.AddCommand(watchCmd)
}
