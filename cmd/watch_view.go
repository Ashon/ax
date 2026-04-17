package cmd

import (
	"fmt"
	"strings"
	"unicode"

	"github.com/charmbracelet/lipgloss"
	xansi "github.com/charmbracelet/x/ansi"
)

// Styles
var (
	sidebarStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	selectedStyle   = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("6"))
	unselectedStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("7"))
	headerStyle     = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("6"))
	borderClr       = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	panelHelpStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
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
	tokenBorderClr  = lipgloss.NewStyle().Foreground(lipgloss.Color("13"))
	tokenTitleSty   = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("13"))
	tokenHighClr    = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("9"))
	tokenNormalClr  = lipgloss.NewStyle().Foreground(lipgloss.Color("7"))
	tokenDimClr     = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	tokenSumClr     = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("6"))
)

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

	streamH := streamPaneHeight(m.height, m.stream)
	contentH := m.height - streamH - lipgloss.Height(footer)
	if contentH < watchMessagePaneMinHeight {
		contentH = watchMessagePaneMinHeight
	}

	// Top area: either the agent card grid (default) or a full-width tmux
	// stream monitor triggered by the Stream quick action.
	var top string
	switch m.viewMode {
	case viewModeStream:
		top = m.renderStreamMonitor(m.width, contentH)
	default:
		top = m.renderAgentGrid(m.width, contentH)
	}

	// === Stream pane (messages, tasks, or tokens) ===
	stream := m.renderSelectedStream(m.width, streamH)

	parts := []string{top}
	if stream != "" {
		parts = append(parts, stream)
	}
	parts = append(parts, footer)

	return lipgloss.JoinVertical(lipgloss.Left, parts...)
}

// renderStreamMonitor shows the focused agent's tmux capture in full width so
// operators can attach to one workspace for real-time monitoring without
// losing the rest of the TUI (footer + optional stream pane remain).
func (m watchModel) renderStreamMonitor(w, h int) string {
	if m.selected < 0 || m.selected >= len(m.sessions) {
		return m.renderAgentGrid(w, h)
	}
	session := m.sessions[m.selected]
	title := headerStyle.Render(fmt.Sprintf(" %s · streaming ", session.Workspace))
	innerW := w - 2
	innerH := h - 2
	topLine := renderPanelTopBorder(borderClr, title, innerW, watchStreamMonitorHelpCandidates()...)
	body := m.renderMainBody(m.captures[session.Workspace], innerW, innerH)
	botLine := borderClr.Render("╰" + strings.Repeat("─", innerW) + "╯")
	all := []string{topLine}
	all = append(all, body...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

func watchStreamMonitorHelpCandidates() []string {
	return []string{
		"esc back · ↑↓ agent · x interrupt · tab stream · q quit",
		"esc back · ↑↓ · x · tab · q",
		"esc back",
	}
}

// renderMainBody extracts the bounded body-rendering of renderMain so the
// stream monitor can reuse it without duplicating the padding/truncation
// logic.
func (m watchModel) renderMainBody(content string, innerW, innerH int) []string {
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
	body := make([]string, 0, innerH)
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
		body = append(body, borderClr.Render("│")+line+strings.Repeat(" ", padding)+borderClr.Render("│"))
	}
	return body
}

func (m watchModel) renderFooter(totalW int) string {
	summary := footerSummarySt.Render(fitDisplayText(m.footerTokenSummary(totalW), totalW))
	helpText := m.quickActionHelpText()
	if m.streamOnly {
		helpText = " [/ ] task · f filter · tab msgs/tasks/tokens · q quit"
	}
	help := lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Render(
		fitDisplayText(helpText, totalW))
	if m.noticeText == "" {
		return lipgloss.JoinVertical(lipgloss.Left, summary, help)
	}
	notice := m.quickActionNoticeStyle().Render(fitDisplayText(" "+m.noticeText, totalW))
	return lipgloss.JoinVertical(lipgloss.Left, summary, notice, help)
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

func renderPanelHelpSegment(width int, candidates ...string) string {
	if width < 3 {
		return ""
	}
	helpText := firstFittingDisplay(width-2, candidates...)
	if helpText == "" {
		return ""
	}
	help := panelHelpStyle.Render(" " + helpText + " ")
	if lipgloss.Width(help) > width {
		return ""
	}
	return help
}

func renderPanelTopBorder(borderStyle lipgloss.Style, title string, innerW int, helpCandidates ...string) string {
	help := renderPanelHelpSegment(innerW-lipgloss.Width(title)-2, helpCandidates...)
	fill := innerW - lipgloss.Width(help) - lipgloss.Width(title) - 1
	if fill < 0 {
		help = ""
		fill = innerW - lipgloss.Width(title) - 1
	}
	if fill < 0 {
		fill = 0
	}
	return borderStyle.Render("╭─") + title + borderStyle.Render(strings.Repeat("─", fill)) + help + borderStyle.Render("╮")
}

func watchAgentsPanelHelpCandidates(menuOpen bool) []string {
	if menuOpen {
		return []string{
			"↑↓ action · enter run · esc close",
			"↑↓ action · enter · esc",
			"↑↓ action",
		}
	}
	return []string{
		"↑↓ agent · enter actions · x",
		"↑↓ agent · enter · x",
		"↑↓ agent",
	}
}

func watchMainPanelHelpCandidates() []string {
	return []string{
		"↑↓ agent · x interrupt · tab stream · q quit",
		"↑↓ agent · x · tab · q",
		"↑↓ agent · x",
	}
}

func watchMessagePanelHelpCandidates(streamOnly bool) []string {
	if streamOnly {
		return []string{
			"tab tasks/tokens · q quit",
			"tab tasks/tokens · q",
			"tab · q",
		}
	}
	return []string{
		"tab tasks/tokens/off · q quit",
		"tab tasks/tokens/off · q",
		"tab · q",
	}
}

func watchTaskPanelHelpCandidates(streamOnly bool) []string {
	if streamOnly {
		return []string{
			"[/] task · f filter · tab msgs/tokens · q quit",
			"[/] task · f filter · tab · q",
			"[/] task · f · tab · q",
			"[/] · f · tab",
		}
	}
	return []string{
		"[/] task · f filter · tab msgs/tokens/off · q quit",
		"[/] task · f filter · tab · q",
		"[/] task · f · tab · q",
		"[/] · f · tab",
	}
}

func watchTokenPanelHelpCandidates(streamOnly bool) []string {
	if streamOnly {
		return []string{
			"tab msgs/tasks · q quit",
			"tab msgs/tasks · q",
			"tab · q",
		}
	}
	return []string{
		"tab msgs/tasks/off · q quit",
		"tab msgs/tasks/off · q",
		"tab · q",
	}
}

func (m watchModel) renderMain(ws, content string, w, h int) string {
	innerW := w - 2 // subtract left + right border
	innerH := h - 2

	// Title
	title := headerStyle.Render(fmt.Sprintf(" %s ", ws))
	topLine := renderPanelTopBorder(borderClr, title, innerW, watchMainPanelHelpCandidates()...)

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
