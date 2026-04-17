package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/charmbracelet/lipgloss"
)

const sidebarRecentActivityWindow = 5 * time.Second

var (
	sidebarRunningFrames = []string{"⠁", "⠃", "⠇", "⠧", "⠷", "⠿", "⠷", "⠧", "⠇", "⠃"}
)

const (
	sidebarAgentStateOffline = "offline"
	sidebarAgentStateIdle    = "idle"
	sidebarAgentStateRunning = "running"
)

type sidebarTreeNode struct {
	name         string
	sessionIndex int
	children     map[string]*sidebarTreeNode
}

type workspaceAttention struct {
	Stale    int
	Diverged int
	Queued   int
}

type sidebarOverlayRow struct {
	text  string
	style lipgloss.Style
}

func (m watchModel) renderSidebar(w, h int) string {
	innerW := w - 2
	innerH := h - 2
	attentionByWorkspace := summarizeWorkspaceAttention(m.tasks)
	selectedLevel := 0
	selectedLineIndex := -1

	// Title
	title := headerStyle.Render(" agents ")
	topLine := renderPanelTopBorder(borderClr, title, innerW, watchAgentsPanelHelpCandidates(m.quickActionsOpen)...)

	var lines []string
	stateNow := m.dataRefreshedAt
	if stateNow.IsZero() {
		stateNow = time.Now()
	}
	for _, entry := range buildSidebarEntriesCached(m.sessions) {
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
			stateLabel := "offline"
			if entry.reconcile != "" {
				stateLabel = entry.reconcile
			}
			left = "  " + strings.Repeat("  ", entry.level) + renderSidebarStateMarker(sidebarAgentStateOffline, m.spinnerTick) + " " + dimStyle.Render(entry.label)
			right = formatSidebarRowMeta(stateLabel, "", attention, max(0, innerW-lipgloss.Width(left)-1))
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
				selectedLevel = entry.level
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
		if entry.sessionIndex == m.selected {
			selectedLineIndex = len(lines) - 1
			if !m.quickActionsOpen && secondary != "" {
				prefix := "    " + strings.Repeat("  ", entry.level)
				secondaryLine := prefix + fitDisplayText(secondary, max(0, innerW-lipgloss.Width(prefix)))
				lines = append(lines, renderWatchSidebarLine(selectedStyle.Render(secondaryLine), "", innerW))
			}
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
	if m.quickActionsOpen {
		lines = overlaySidebarFrom(lines, m.renderSelectedQuickActionLines(selectedLevel, innerW), selectedLineIndex+1)
	}

	botLine := borderClr.Render("╰" + strings.Repeat("─", innerW) + "╯")

	all := []string{topLine}
	all = append(all, lines...)
	all = append(all, botLine)
	return strings.Join(all, "\n")
}

func overlaySidebarFrom(lines, overlay []string, start int) []string {
	if len(lines) == 0 || len(overlay) == 0 || start < 0 || start >= len(lines) {
		return lines
	}
	if maxLines := len(lines) - start; len(overlay) > maxLines {
		overlay = overlay[:maxLines]
	}
	copy(lines[start:], overlay)
	return lines
}

func buildSidebarEntries(sessions []tmux.SessionInfo) []sidebarEntry {
	// Try config-driven tree first; fall back to name-based splitting
	// when no config is available.
	if cfgPath, err := resolveConfigPath(); err == nil {
		if tree, err := config.LoadTree(cfgPath); err == nil && tree != nil {
			topology, topoErr := loadTeamReconfigureTopology(cfgPath)
			if topoErr == nil {
				return buildSidebarFromTree(tree, sessions, topology.Enabled, topology.Desired)
			}
			return buildSidebarFromTree(tree, sessions, false, nil)
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

func (m watchModel) renderSelectedQuickActionLines(level, innerW int) []string {
	prefix := "    " + strings.Repeat("  ", level)
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

func renderWatchSidebarOverlay(prefix string, innerW int, titleCandidates []string, rows []sidebarOverlayRow) []string {
	prefixW := lipgloss.Width(prefix)
	contentW := innerW - prefixW - 2
	if contentW <= 0 {
		lines := make([]string, 0, len(rows))
		for _, row := range rows {
			raw := prefix + fitDisplayText(row.text, max(0, innerW-prefixW))
			lines = append(lines, renderWatchSidebarLine(row.style.Render(raw), "", innerW))
		}
		return lines
	}

	title := ""
	if titleText := firstFittingDisplay(max(0, contentW-3), titleCandidates...); titleText != "" {
		title = headerStyle.Render(" " + titleText + " ")
	}

	lines := []string{
		renderWatchSidebarLine(prefix+renderPanelTopBorder(borderClr, title, contentW), "", innerW),
	}
	for _, row := range rows {
		text := fitDisplayText(row.text, contentW)
		padding := contentW - lipgloss.Width(text)
		line := prefix + borderClr.Render("│") + row.style.Render(text) + strings.Repeat(" ", padding) + borderClr.Render("│")
		lines = append(lines, renderWatchSidebarLine(line, "", innerW))
	}
	lines = append(lines, renderWatchSidebarLine(prefix+borderClr.Render("╰"+strings.Repeat("─", contentW)+"╯"), "", innerW))
	return lines
}

// buildSidebarFromTree renders a project tree into sidebar entries.
// Each project becomes a group header. Its orchestrator is the first
// leaf under it, followed by workspaces, then nested projects.
// Running sessions not in the tree are appended under a runtime-only /
// unregistered group so they stay visible.
func buildSidebarFromTree(tree *config.ProjectNode, sessions []tmux.SessionInfo, reconfigureEnabled bool, desired map[string]bool) []sidebarEntry {
	sessionByWorkspace := make(map[string]int, len(sessions))
	for i, s := range sessions {
		sessionByWorkspace[s.Workspace] = i
	}

	known := make(map[string]bool)
	collectKnownFromTree(tree, known)

	var entries []sidebarEntry
	appendProjectEntries(tree, 0, sessionByWorkspace, &entries, desired)

	// Append any running session that wasn't part of the config tree
	var unregistered []int
	for i, s := range sessions {
		if !known[s.Workspace] {
			unregistered = append(unregistered, i)
		}
	}
	if len(unregistered) > 0 {
		entries = append(entries, sidebarEntry{
			label: runtimeOnlyGroupLabel(reconfigureEnabled),
			group: true,
			level: 0,
		})
		for _, idx := range unregistered {
			name := sessions[idx].Workspace
			entries = append(entries, sidebarEntry{
				label:        name,
				workspace:    name,
				reconcile:    reconfigureSidebarState(name, desired, true, false),
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
	if rootOrchestratorVisible(node) {
		orchName := "orchestrator"
		if node.Prefix != "" {
			orchName = node.Prefix + ".orchestrator"
		}
		known[orchName] = true
	}
	for _, ws := range node.Workspaces {
		known[ws.MergedName] = true
	}
	for _, child := range node.Children {
		collectKnownFromTree(child, known)
	}
}

func appendProjectEntries(node *config.ProjectNode, level int, sessionByWorkspace map[string]int, entries *[]sidebarEntry, desired map[string]bool) {
	if node == nil {
		return
	}

	*entries = append(*entries, sidebarEntry{
		label: "▾ " + node.DisplayName(),
		group: true,
		level: level,
	})

	// Project orchestrator first
	if rootOrchestratorVisible(node) {
		orchName := "orchestrator"
		if node.Prefix != "" {
			orchName = node.Prefix + ".orchestrator"
		}
		orchLabel := "◆ orchestrator"
		orchReconcile := reconfigureSidebarState(orchName, desired, false, false)
		if node.Prefix == "" && orchName == "orchestrator" {
			orchReconcile = ""
		}
		if idx, ok := sessionByWorkspace[orchName]; ok {
			*entries = append(*entries, sidebarEntry{
				label:        orchLabel,
				workspace:    orchName,
				reconcile:    reconfigureSidebarState(orchName, desired, true, false),
				sessionIndex: idx,
				level:        level + 1,
			})
		} else {
			*entries = append(*entries, sidebarEntry{
				label:        orchLabel,
				workspace:    orchName,
				reconcile:    orchReconcile,
				sessionIndex: -1,
				level:        level + 1,
			})
		}
	}

	for _, ws := range node.Workspaces {
		idx, ok := sessionByWorkspace[ws.MergedName]
		if !ok {
			*entries = append(*entries, sidebarEntry{
				label:        ws.Name,
				workspace:    ws.MergedName,
				reconcile:    reconfigureSidebarState(ws.MergedName, desired, false, false),
				sessionIndex: -1,
				level:        level + 1,
			})
			continue
		}
		*entries = append(*entries, sidebarEntry{
			label:        ws.Name,
			workspace:    ws.MergedName,
			reconcile:    reconfigureSidebarState(ws.MergedName, desired, true, false),
			sessionIndex: idx,
			level:        level + 1,
		})
	}

	for _, child := range node.Children {
		appendProjectEntries(child, level+1, sessionByWorkspace, entries, desired)
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

	if cfg.DisableRootOrchestrator {
		delete(runtimes, "orchestrator")
	} else {
		runtimes["orchestrator"] = agent.NormalizeRuntime(cfg.OrchestratorRuntime)
	}
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
