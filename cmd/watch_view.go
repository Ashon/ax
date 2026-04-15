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
