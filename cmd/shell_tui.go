package cmd

import (
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
)

type inputMode int

const (
	modeInput   inputMode = iota
	modeControl
)

var modeInputStyle = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("2"))
var modeControlStyle = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("3"))

type shellModel struct {
	width      int
	height     int
	selected   int
	captures   map[string]string
	prevCaps   map[string]string
	activity   map[string]time.Time
	sessions   []tmux.SessionInfo
	runtimes   map[string]string
	msgHistory []daemon.HistoryEntry
	histPath   string
	showStream bool

	mode        inputMode
	viewTarget  string // workspace shown in main pane
	orchSession string // tmux session name for orchestrator
}

func newShellModel(orchSession, socketPath string) shellModel {
	return shellModel{
		captures:    make(map[string]string),
		prevCaps:    make(map[string]string),
		activity:    make(map[string]time.Time),
		runtimes:    loadWatchRuntimes(),
		histPath:    daemon.HistoryFilePath(socketPath),
		showStream:  true,
		mode:        modeInput,
		viewTarget:  "orchestrator",
		orchSession: orchSession,
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
	case tickMsg:
		m.sessions, _ = tmux.ListSessions()
		m.selected = clampSelection(m.selected, m.sessions)

		// Resize the viewed pane's tmux window to match main panel
		viewSession := m.currentViewSession()
		if viewSession != "" && m.width > 0 {
			sideW := watchSidebarWidth
			mainW := m.width - sideW - 2
			streamH := messagePaneHeight(m.height, m.showStream)
			mainH := m.height - streamH - 3
			if mainW > 10 && mainH > 5 {
				resizeTmuxWindow(viewSession, mainW, mainH)
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

func (m shellModel) handleInputMode(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	if msg.String() == "ctrl+a" {
		m.mode = modeControl
		return m, nil
	}
	return m, m.forwardKey(msg)
}

func (m shellModel) handleControlMode(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	m.mode = modeInput

	switch msg.String() {
	case "q":
		return m, tea.Quit
	case "k", "up":
		m.selected = moveSelection(m.selected, m.sessions, -1)
	case "j", "down":
		m.selected = moveSelection(m.selected, m.sessions, 1)
	case "t":
		m.showStream = !m.showStream
	case "x":
		if m.selected < len(m.sessions) {
			_ = tmux.InterruptWorkspace(m.sessions[m.selected].Workspace)
		}
	case "v":
		if m.selected < len(m.sessions) {
			m.viewTarget = m.sessions[m.selected].Workspace
		}
	case "o":
		m.viewTarget = "orchestrator"
	case "ctrl+a":
		session := m.currentViewSession()
		if session != "" {
			return m, func() tea.Msg {
				tmux.SendSpecialKeyToSession(session, "C-a")
				return nil
			}
		}
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

func (m shellModel) View() string {
	if m.width == 0 || len(m.sessions) == 0 {
		return "Loading... (waiting for sessions)"
	}

	sideW := watchSidebarWidth
	mainW := m.width - sideW
	if mainW < 20 {
		mainW = 20
	}

	streamH := messagePaneHeight(m.height, m.showStream)
	contentH := m.height - streamH - 1

	// Sidebar — reuse watch rendering with shell's state
	sidebar := m.renderSidebar(sideW, contentH)

	// Main pane — show viewTarget's capture
	var mainContent string
	content := m.captures[m.viewTarget]
	mainContent = m.renderMain(m.viewTarget, content, mainW, contentH)

	top := lipgloss.JoinHorizontal(lipgloss.Top, sidebar, mainContent)

	var stream string
	if m.showStream {
		stream = m.renderStream(m.width, streamH)
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
				"j/k select · v view · o orchestrator · t stream · x interrupt · q quit")
	}
	return modeInputStyle.Render(" [INPUT] ") +
		lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Render(
			"Ctrl+A: control mode")
}

// Delegate rendering to watch.go's functions via wrapper methods

func (m shellModel) renderSidebar(w, h int) string {
	wm := watchModel{
		width:    m.width,
		height:   m.height,
		selected: m.selected,
		captures: m.captures,
		prevCaps: m.prevCaps,
		activity: m.activity,
		sessions: m.sessions,
		runtimes: m.runtimes,
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

