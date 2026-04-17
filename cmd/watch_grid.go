package cmd

import (
	"fmt"
	"strings"
	"time"

	"github.com/charmbracelet/lipgloss"
)

// Agent cards replace the old narrow sidebar list. A card bundles the agent
// identity (name + marker), runtime, live token summary, attention badges,
// and a short status preview. Cards flow in a responsive grid across the
// width of the screen and group under their project headers.
const (
	watchCardWidth       = 34
	watchCardHeight      = 6 // top border + 4 content rows + bottom border
	watchCardGapH        = 1
	watchCardGapV        = 1
	watchGridPaddingLeft = 1
	watchGridContentRows = 4
)

var (
	watchCardBorderColor         = lipgloss.Color("8")
	watchCardSelectedBorderColor = lipgloss.Color("6")
	watchGridGroupStyle          = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("6"))
	watchCardNameStyle           = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("7"))
	watchCardSelectedNameStyle   = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("6"))
	watchCardRuntimeStyle        = lipgloss.NewStyle().Foreground(lipgloss.Color("10"))
	watchCardTokensStyle         = lipgloss.NewStyle().Foreground(lipgloss.Color("13"))
	watchCardAttentionStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("9"))
	watchCardDimStyle            = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	watchCardStatusStyle         = lipgloss.NewStyle().Foreground(lipgloss.Color("7"))
	watchCardSelectedStatusStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("6"))
)

// renderAgentGrid draws the full-width agent panel. It returns the rendered
// string sized to exactly w columns and h rows. Group headers occupy one row
// each; agent cards occupy watchCardHeight rows each and wrap into as many
// columns as fit at watchCardWidth + watchCardGapH per card.
func (m watchModel) renderAgentGrid(w, h int) string {
	innerW := w - 2
	innerH := h - 2
	if innerW < 1 {
		innerW = 1
	}
	if innerH < 1 {
		innerH = 1
	}

	title := headerStyle.Render(" agents ")
	topLine := renderPanelTopBorder(borderClr, title, innerW, watchAgentsPanelHelpCandidates(m.quickActionsOpen)...)
	botLine := borderClr.Render("╰" + strings.Repeat("─", innerW) + "╯")

	attentionByWorkspace := summarizeWorkspaceAttention(m.tasks)
	stateNow := m.dataRefreshedAt
	if stateNow.IsZero() {
		stateNow = time.Now()
	}

	// Walk cached sidebar entries (already grouped + ordered) and flatten into
	// a list of grid rows. Each row is either a group header or a wrapping
	// line of agent cards.
	cards := make([]agentCardContent, 0, 16)
	items := make([]flatItem, 0, 32)
	for _, entry := range buildSidebarEntriesCached(m.sessions) {
		if entry.group {
			items = append(items, flatItem{group: entry.label, level: entry.level})
			continue
		}
		card := m.buildAgentCard(entry, attentionByWorkspace, stateNow)
		cards = append(cards, card)
		items = append(items, flatItem{card: &cards[len(cards)-1], level: entry.level})
	}

	lines := renderAgentGridBody(items, innerW, innerH, m.selected, m.spinnerTick)
	// Pad to full height BEFORE overlaying quick actions so the overlay has
	// enough underlying rows to write into.
	for len(lines) < innerH {
		lines = append(lines, borderClr.Render("│")+strings.Repeat(" ", innerW)+borderClr.Render("│"))
	}
	if len(lines) > innerH {
		lines = lines[:innerH]
	}
	if m.quickActionsOpen {
		lines = m.overlayQuickActionsOnGrid(items, lines, innerW)
	}

	all := []string{topLine}
	all = append(all, lines...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

// agentCardContent is the pre-computed payload used to render one card.
// Keeping it in a struct rather than closing over model state lets us reuse
// the same card for both the normal grid rendering and the quick-action
// overlay preview line.
type agentCardContent struct {
	sessionIndex int
	workspace    string
	label        string
	runtime      string
	reconcile    string
	state        string // running | idle | offline
	attention    string
	tokens       string
	statusLine   string
	offline      bool
}

func (m watchModel) buildAgentCard(entry sidebarEntry, attention map[string]workspaceAttention, now time.Time) agentCardContent {
	workspaceName := entry.workspace
	if workspaceName == "" && entry.sessionIndex >= 0 && entry.sessionIndex < len(m.sessions) {
		workspaceName = m.sessions[entry.sessionIndex].Workspace
	}
	card := agentCardContent{
		sessionIndex: entry.sessionIndex,
		workspace:    workspaceName,
		label:        entry.label,
		reconcile:    entry.reconcile,
		attention:    workspaceAttentionBadge(attention[workspaceName]),
	}
	if entry.sessionIndex < 0 || entry.sessionIndex >= len(m.sessions) {
		card.offline = true
		card.state = sidebarAgentStateOffline
		if entry.reconcile != "" {
			card.runtime = entry.reconcile
		} else {
			card.runtime = "offline"
		}
		return card
	}

	session := m.sessions[entry.sessionIndex]
	card.runtime = m.runtimes[session.Workspace]
	card.state = m.sidebarStates[session.Workspace]
	if card.state == "" {
		card.state = deriveSidebarAgentState(m.captures[session.Workspace], m.activity[session.Workspace], now)
	}
	card.tokens = formatSidebarTokenSummary(m.tokenData[session.Workspace], watchCardWidth-4)
	statusText := workspaceStatusPreview(m.workspaceInfos, session.Workspace, watchCardWidth-2)
	if statusText == "" {
		statusText = parseAgentStatus(m.captures[session.Workspace])
	}
	card.statusLine = statusText
	return card
}

// renderAgentGridBody fills the interior of the agents panel with groups and
// wrapping rows of cards, fully paddted to innerW. Cards adopt a distinct
// border color when they match the currently selected session index.
func renderAgentGridBody(items []flatItem, innerW, innerH, selected, tick int) []string {
	columns := max(1, (innerW-watchGridPaddingLeft)/(watchCardWidth+watchCardGapH))
	lines := make([]string, 0, innerH)
	row := make([]agentCardContent, 0, columns)
	rowSelectedIdx := -1

	flushRow := func() {
		if len(row) == 0 {
			return
		}
		cardLines := renderCardsRow(row, innerW, selected, tick, rowSelectedIdx)
		lines = append(lines, cardLines...)
		// gap row after every card row.
		gap := borderClr.Render("│") + strings.Repeat(" ", innerW) + borderClr.Render("│")
		for i := 0; i < watchCardGapV; i++ {
			lines = append(lines, gap)
		}
		row = row[:0]
		rowSelectedIdx = -1
	}

	for _, item := range items {
		if item.card == nil {
			// Group header: flush current cards row first, then render header.
			flushRow()
			header := padSidebarLine(
				strings.Repeat("  ", item.level)+watchGridGroupStyle.Render(item.group),
				innerW,
			)
			lines = append(lines, header)
			continue
		}
		row = append(row, *item.card)
		if item.card.sessionIndex == selected {
			rowSelectedIdx = len(row) - 1
		}
		if len(row) >= columns {
			flushRow()
		}
	}
	flushRow()
	return lines
}

type flatItem struct {
	group string
	card  *agentCardContent
	level int
}

func padSidebarLine(content string, innerW int) string {
	width := lipgloss.Width(content)
	pad := innerW - width
	if pad < 0 {
		content = fitDisplayText(content, innerW)
		pad = innerW - lipgloss.Width(content)
	}
	if pad < 0 {
		pad = 0
	}
	return borderClr.Render("│") + " " + content + strings.Repeat(" ", pad-1) + borderClr.Render("│")
}

// renderCardsRow renders a full horizontal line of cards, joined side-by-side
// with single-column gaps, surrounded by the panel borders on each side. The
// returned slice has watchCardHeight entries.
func renderCardsRow(cards []agentCardContent, innerW, selected, tick, rowSelectedIdx int) []string {
	_ = rowSelectedIdx
	cardBlocks := make([]string, 0, len(cards))
	for _, card := range cards {
		cardBlocks = append(cardBlocks, renderAgentCard(card, card.sessionIndex == selected, tick))
	}

	cardLines := make([]string, watchCardHeight)
	for rowIdx := 0; rowIdx < watchCardHeight; rowIdx++ {
		parts := make([]string, 0, len(cardBlocks))
		for _, block := range cardBlocks {
			blockLines := strings.Split(block, "\n")
			if rowIdx < len(blockLines) {
				parts = append(parts, blockLines[rowIdx])
			} else {
				parts = append(parts, strings.Repeat(" ", watchCardWidth))
			}
		}
		joined := strings.Join(parts, strings.Repeat(" ", watchCardGapH))
		leadPad := watchGridPaddingLeft
		trailW := innerW - leadPad - lipgloss.Width(joined)
		if trailW < 0 {
			joined = fitDisplayText(joined, innerW-leadPad)
			trailW = innerW - leadPad - lipgloss.Width(joined)
		}
		if trailW < 0 {
			trailW = 0
		}
		cardLines[rowIdx] = borderClr.Render("│") + strings.Repeat(" ", leadPad) + joined + strings.Repeat(" ", trailW) + borderClr.Render("│")
	}
	return cardLines
}

// renderAgentCard builds the watchCardWidth × watchCardHeight ASCII card for
// a single agent. Layout:
//
//	╭ marker label       runtime  ╮
//	│ ↑… ↓… $cost                  │
//	│ D1 S2                        │
//	│ thinking…                    │
//	╰──────────────────────────────╯
func renderAgentCard(card agentCardContent, selected bool, tick int) string {
	width := watchCardWidth
	borderStyle := lipgloss.NewStyle().Foreground(watchCardBorderColor)
	if selected {
		borderStyle = lipgloss.NewStyle().Foreground(watchCardSelectedBorderColor).Bold(true)
	}
	nameStyle := watchCardNameStyle
	statusStyle := watchCardStatusStyle
	if selected {
		nameStyle = watchCardSelectedNameStyle
		statusStyle = watchCardSelectedStatusStyle
	}
	if card.offline {
		nameStyle = watchCardDimStyle
	}

	// Row 1: marker · label (left) + runtime (right)
	marker := renderSidebarStateMarker(card.state, tick)
	labelText := card.label
	rightText := ""
	if card.runtime != "" {
		rightText = watchCardRuntimeStyle.Render(card.runtime)
		if card.offline {
			rightText = watchCardDimStyle.Render(card.runtime)
		}
	}
	row1 := joinCardRow(" "+marker+" "+nameStyle.Render(labelText), rightText, width-2)

	// Row 2: tokens line. Prefer tokens; fall back to reconcile/status text
	// on offline cards where tokens are unavailable.
	var row2 string
	switch {
	case card.tokens != "":
		row2 = " " + watchCardTokensStyle.Render(card.tokens)
	case card.offline && card.reconcile != "":
		row2 = " " + watchCardDimStyle.Render(card.reconcile)
	default:
		row2 = ""
	}

	// Row 3: attention badges.
	row3 := ""
	if card.attention != "" {
		row3 = " " + watchCardAttentionStyle.Render(card.attention)
	}

	// Row 4: status preview.
	row4 := ""
	if card.statusLine != "" {
		row4 = " " + statusStyle.Render(truncateStr(card.statusLine, width-3))
	}

	interior := width - 2
	rows := []string{
		padCardInterior(row1, interior),
		padCardInterior(row2, interior),
		padCardInterior(row3, interior),
		padCardInterior(row4, interior),
	}

	topBorder := borderStyle.Render("╭" + strings.Repeat("─", width-2) + "╮")
	botBorder := borderStyle.Render("╰" + strings.Repeat("─", width-2) + "╯")
	out := []string{topBorder}
	for _, r := range rows {
		out = append(out, borderStyle.Render("│")+r+borderStyle.Render("│"))
	}
	out = append(out, botBorder)
	return strings.Join(out, "\n")
}

func joinCardRow(left, right string, width int) string {
	lw := lipgloss.Width(left)
	rw := lipgloss.Width(right)
	if right == "" {
		return left
	}
	if lw+1+rw > width {
		// Truncate right side.
		maxR := max(0, width-lw-1)
		right = fitDisplayText(right, maxR)
		rw = lipgloss.Width(right)
	}
	gap := width - lw - rw
	if gap < 1 {
		gap = 1
	}
	return left + strings.Repeat(" ", gap) + right
}

func padCardInterior(content string, width int) string {
	current := lipgloss.Width(content)
	if current > width {
		content = fitDisplayText(content, width)
		current = lipgloss.Width(content)
	}
	if current < width {
		content = content + strings.Repeat(" ", width-current)
	}
	return content
}

// overlayQuickActionsOnGrid patches the quick-action overlay into the grid
// body just under the selected card, emulating the sidebar behaviour.
func (m watchModel) overlayQuickActionsOnGrid(items []flatItem, lines []string, innerW int) []string {
	if !m.quickActionsOpen {
		return lines
	}
	// Locate selected card's row in the rendered output. Group headers take
	// 1 row; each agent row takes watchCardHeight + watchCardGapV rows. We
	// walk items accumulating rows until we hit the selected card.
	rowAcc := 0
	columns := max(1, (innerW-watchGridPaddingLeft)/(watchCardWidth+watchCardGapH))
	var rowItems []flatItem
	flushLen := 0
	for _, item := range items {
		if item.card == nil {
			if len(rowItems) > 0 {
				rowAcc += flushLen
				rowItems = nil
				flushLen = 0
			}
			rowAcc++
			continue
		}
		rowItems = append(rowItems, item)
		if item.card.sessionIndex == m.selected {
			flushLen = watchCardHeight + watchCardGapV
			break
		}
		if len(rowItems) >= columns {
			rowAcc += watchCardHeight + watchCardGapV
			rowItems = nil
		}
	}
	if flushLen == 0 {
		return lines
	}
	overlayStart := rowAcc + watchCardHeight
	if overlayStart >= len(lines) {
		return lines
	}
	overlayRows := m.renderGridQuickActionOverlay(innerW)
	return overlaySidebarFrom(lines, overlayRows, overlayStart)
}

func (m watchModel) renderGridQuickActionOverlay(innerW int) []string {
	prefix := " " + strings.Repeat(" ", watchGridPaddingLeft)
	workspaceName := m.selectedWorkspaceName()
	if m.quickActionConfirm {
		action, ok := m.selectedQuickAction()
		if !ok {
			return nil
		}
		return renderWatchSidebarOverlay(prefix, innerW, []string{
			strings.TrimSpace(workspaceName + " " + strings.ToLower(action.Label)),
			"confirm",
		}, []sidebarOverlayRow{
			{text: action.confirmationPrompt(), style: selectedStyle},
			{text: "enter confirm · esc cancel", style: selectedStyle},
		})
	}
	start, end := m.quickActionViewport()
	rows := make([]sidebarOverlayRow, 0, end-start)
	for i := start; i < end; i++ {
		action := m.quickActions[i]
		raw := "  " + action.Label
		style := unselectedStyle
		if i == clampIndex(m.quickActionSelected, len(m.quickActions)) {
			raw = "▸ " + action.Label
			style = selectedStyle
		}
		rows = append(rows, sidebarOverlayRow{text: raw, style: style})
	}
	return renderWatchSidebarOverlay(prefix, innerW, []string{
		strings.TrimSpace(fmt.Sprintf("%s actions %d/%d", workspaceName, m.quickActionSelected+1, len(m.quickActions))),
		fmt.Sprintf("actions %d/%d", m.quickActionSelected+1, len(m.quickActions)),
		"actions",
	}, rows)
}
