package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/ashon/amux/internal/daemon"
	"github.com/ashon/amux/internal/tmux"
	"github.com/spf13/cobra"
)

var watchCmd = &cobra.Command{
	Use:   "watch",
	Short: "Monitor all workspace sessions with message stream (read-only TUI)",
	RunE: func(cmd *cobra.Command, args []string) error {
		p := tea.NewProgram(newWatchModel(), tea.WithAltScreen())
		_, err := p.Run()
		return err
	},
}

var (
	borderColor = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	activeColor = lipgloss.NewStyle().Foreground(lipgloss.Color("6")).Bold(true)
	streamFrom  = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("3"))
	streamTo    = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("2"))
	streamTime  = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	msgBorder   = lipgloss.NewStyle().Foreground(lipgloss.Color("5"))
	msgTitle    = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("5"))
)

type tickMsg time.Time

type watchModel struct {
	width      int
	height     int
	captures   map[string]string
	sessions   []tmux.SessionInfo
	msgHistory []daemon.HistoryEntry
	histPath   string
}

func newWatchModel() watchModel {
	return watchModel{
		captures: make(map[string]string),
		histPath: daemon.HistoryFilePath(socketPath),
	}
}

func (m watchModel) Init() tea.Cmd {
	return tea.Batch(tickCmd(), tea.WindowSize())
}

func (m watchModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "q", "ctrl+c", "esc":
			return m, tea.Quit
		}
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		return m, nil
	case tickMsg:
		m.sessions, _ = tmux.ListSessions()
		for _, s := range m.sessions {
			m.captures[s.Workspace] = capturePane(s.Name)
		}
		m.msgHistory = readHistoryFile(m.histPath, 50)
		return m, tickCmd()
	}
	return m, nil
}

func (m watchModel) View() string {
	if m.width == 0 || len(m.sessions) == 0 {
		return "Loading..."
	}

	streamH := m.height / 4
	if streamH < 4 {
		streamH = 4
	}
	gridH := m.height - streamH

	n := len(m.sessions)
	gridCols, gridRows := gridLayout(n)

	// Calculate per-column widths and per-row heights to fill screen exactly
	baseW := m.width / gridCols
	extraW := m.width % gridCols
	baseH := gridH / gridRows
	extraH := gridH % gridRows

	colWidths := make([]int, gridCols)
	for c := 0; c < gridCols; c++ {
		colWidths[c] = baseW
		if c < extraW {
			colWidths[c]++
		}
	}
	rowHeights := make([]int, gridRows)
	for r := 0; r < gridRows; r++ {
		rowHeights[r] = baseH
		if r < extraH {
			rowHeights[r]++
		}
	}

	var gridRows2 []string
	for row := 0; row < gridRows; row++ {
		var paneStrs []string
		for col := 0; col < gridCols; col++ {
			idx := row*gridCols + col
			outerW := colWidths[col]
			outerH := rowHeights[row]
			innerW := outerW - 2
			innerH := outerH - 2
			if innerW < 5 {
				innerW = 5
			}
			if innerH < 1 {
				innerH = 1
			}
			if idx >= n {
				paneStrs = append(paneStrs, emptyPane(outerW, outerH))
				continue
			}
			ws := m.sessions[idx].Workspace
			content := m.captures[ws]
			paneStrs = append(paneStrs, renderPane(ws, content, innerW, innerH))
		}
		gridRows2 = append(gridRows2, lipgloss.JoinHorizontal(lipgloss.Top, paneStrs...))
	}

	grid := lipgloss.JoinVertical(lipgloss.Left, gridRows2...)

	// Message stream — use full width
	stream := m.renderStream(m.width, streamH)

	return grid + "\n" + stream
}

func renderPane(title, content string, innerW, innerH int) string {
	// Header line: ╭─ title ──...──╮
	titleStr := activeColor.Render(fmt.Sprintf(" %s ", title))
	titleVisW := lipgloss.Width(titleStr)
	padRight := innerW - titleVisW - 1 // -1 for the ─ after ╭
	if padRight < 0 {
		padRight = 0
	}
	topLine := borderColor.Render("╭─") + titleStr + borderColor.Render(strings.Repeat("─", padRight)+"╮")

	// Content lines
	lines := strings.Split(content, "\n")
	// Trim trailing empty
	for len(lines) > 0 && strings.TrimSpace(lines[len(lines)-1]) == "" {
		lines = lines[:len(lines)-1]
	}
	// Take last innerH lines
	if len(lines) > innerH {
		lines = lines[len(lines)-innerH:]
	}

	var bodyLines []string
	for i := 0; i < innerH; i++ {
		line := ""
		if i < len(lines) {
			line = lines[i]
		}
		// Truncate to innerW visible chars, pad with spaces
		visW := lipgloss.Width(line)
		if visW > innerW {
			line = ansiTruncate(line, innerW)
			visW = lipgloss.Width(line)
		}
		padding := innerW - visW
		if padding < 0 {
			padding = 0
		}
		bodyLines = append(bodyLines,
			borderColor.Render("│")+line+strings.Repeat(" ", padding)+borderColor.Render("│"))
	}

	// Bottom line: ╰──...──╯
	botLine := borderColor.Render("╰" + strings.Repeat("─", innerW) + "╯")

	all := []string{topLine}
	all = append(all, bodyLines...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

func emptyPane(outerW, outerH int) string {
	var lines []string
	for i := 0; i < outerH; i++ {
		lines = append(lines, strings.Repeat(" ", outerW))
	}
	return strings.Join(lines, "\n")
}

func (m watchModel) renderStream(totalW, totalH int) string {
	innerW := totalW - 2
	innerH := totalH - 2
	if innerH < 1 {
		innerH = 1
	}

	titleStr := msgTitle.Render(" messages ")
	titleVisW := lipgloss.Width(titleStr)
	padRight := innerW - titleVisW - 1
	if padRight < 0 {
		padRight = 0
	}
	topLine := msgBorder.Render("╭─") + titleStr + msgBorder.Render(strings.Repeat("─", padRight)+"╮")

	var msgLines []string
	start := 0
	if len(m.msgHistory) > innerH {
		start = len(m.msgHistory) - innerH
	}
	for _, entry := range m.msgHistory[start:] {
		ts := streamTime.Render(entry.Timestamp.Format("15:04:05"))
		from := streamFrom.Render(entry.From)
		to := streamTo.Render(entry.To)
		content := strings.ReplaceAll(entry.Content, "\n", " ")
		content = truncateStr(content, innerW-30)
		msgLines = append(msgLines, fmt.Sprintf(" %s %s → %s: %s", ts, from, to, content))
	}
	if len(msgLines) == 0 {
		msgLines = append(msgLines, streamTime.Render("  (no messages yet)"))
	}

	var bodyLines []string
	for i := 0; i < innerH; i++ {
		line := ""
		if i < len(msgLines) {
			line = msgLines[i]
		}
		visW := lipgloss.Width(line)
		padding := innerW - visW
		if padding < 0 {
			padding = 0
		}
		bodyLines = append(bodyLines,
			msgBorder.Render("│")+line+strings.Repeat(" ", padding)+msgBorder.Render("│"))
	}

	botLine := msgBorder.Render("╰" + strings.Repeat("─", innerW) + "╯")

	all := []string{topLine}
	all = append(all, bodyLines...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

// Helpers

func tickCmd() tea.Cmd {
	return tea.Tick(time.Second, func(t time.Time) tea.Msg {
		return tickMsg(t)
	})
}

func capturePane(sessionName string) string {
	out, err := exec.Command("tmux", "capture-pane", "-t", sessionName, "-p", "-e").Output()
	if err != nil {
		return "(capture failed)"
	}
	return string(out)
}

// ansiTruncate truncates a string with ANSI codes to maxW visible characters
func ansiTruncate(s string, maxW int) string {
	visW := 0
	i := 0
	runes := []rune(s)
	for i < len(runes) {
		if runes[i] == '\x1b' {
			// Skip ANSI escape sequence
			j := i + 1
			if j < len(runes) && runes[j] == '[' {
				j++
				for j < len(runes) && !((runes[j] >= 'A' && runes[j] <= 'Z') || (runes[j] >= 'a' && runes[j] <= 'z')) {
					j++
				}
				if j < len(runes) {
					j++ // include the final letter
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

func gridLayout(n int) (cols, rows int) {
	switch {
	case n <= 1:
		return 1, 1
	case n <= 2:
		return 2, 1
	case n <= 4:
		return 2, 2
	case n <= 6:
		return 3, 2
	default:
		return 3, (n + 2) / 3
	}
}

func init() {
	rootCmd.AddCommand(watchCmd)
}
