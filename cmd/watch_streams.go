package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/usage"
	"github.com/charmbracelet/lipgloss"
)

type tokenTotals struct {
	ReportingAgents int
	SessionCount    int
	TotalUp         float64
	TotalDown       float64
	StandaloneTotal float64
	TotalCost       float64
	CostAgents      int
}

func loadWatchTokenTrends(_ []tmux.SessionInfo) (map[string]usage.WorkspaceTrend, bool) {
	sp := daemonutil.ExpandSocketPath(socketPath)
	if !isDaemonRunning(sp) {
		return map[string]usage.WorkspaceTrend{}, true
	}

	dirByWorkspace := loadWatchWorkspaceDirs()
	requests := watchTrendRequests(dirByWorkspace)
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

func watchTrendRequests(dirByWorkspace map[string]string) []daemon.UsageTrendWorkspace {
	workspaces := make([]string, 0, len(dirByWorkspace))
	for workspace, cwd := range dirByWorkspace {
		if strings.TrimSpace(cwd) == "" {
			continue
		}
		workspaces = append(workspaces, workspace)
	}
	sort.Strings(workspaces)
	requests := make([]daemon.UsageTrendWorkspace, 0, len(workspaces))
	for _, workspace := range workspaces {
		requests = append(requests, daemon.UsageTrendWorkspace{
			Workspace: workspace,
			Cwd:       strings.TrimSpace(dirByWorkspace[workspace]),
		})
	}
	return requests
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

// tokenEntriesFromMap keeps only rows with parsed token data and sorts the
// detailed tokens pane by descending cost, then by workspace name for a
// deterministic order when multiple rows share the same cost.
func tokenEntriesFromMap(tokenData map[string]agentTokens) []agentTokens {
	entries := make([]agentTokens, 0, len(tokenData))
	for _, t := range tokenData {
		if t.Up != "" || t.Down != "" || t.Total != "" || t.Cost != "" {
			entries = append(entries, t)
		}
	}
	sort.Slice(entries, func(i, j int) bool {
		costI := parseCostValue(entries[i].Cost)
		costJ := parseCostValue(entries[j].Cost)
		if costI == costJ {
			return entries[i].Workspace < entries[j].Workspace
		}
		return costI > costJ
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

func (m watchModel) renderStream(totalW, totalH int) string {
	innerW := totalW - 2
	innerH := totalH - 2
	if innerH < 1 {
		innerH = 1
	}

	title := msgTitleStyle.Render(" messages ")
	topLine := renderPanelTopBorder(msgBorderClr, title, innerW, watchMessagePanelHelpCandidates(m.streamOnly)...)

	var msgLines []string
	start := 0
	if len(m.msgHistory) > innerH {
		start = len(m.msgHistory) - innerH
	}
	for _, entry := range m.msgHistory[start:] {
		ts := msgTimeStyle.Render(entry.Timestamp.Format("15:04:05"))
		from := msgFromStyle.Render(entry.From)
		to := msgToStyle.Render(entry.To)
		content := sanitizeDisplayLine(strings.ReplaceAll(entry.Content, "\n", " "))
		prefix := fmt.Sprintf(" %s %s → %s: ", ts, from, to)
		contentW := max(0, innerW-lipgloss.Width(prefix))
		line := prefix + fitDisplayText(content, contentW)
		msgLines = append(msgLines, fitDisplayText(line, innerW))
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
			line = fitDisplayText(line, innerW)
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

func formatMCPProxyMetric(total int64) string {
	if total <= 0 {
		return "-"
	}
	return "~" + formatTokenCount(float64(total))
}

func formatPercent(numerator, denominator int64) string {
	if numerator <= 0 || denominator <= 0 {
		return "-"
	}
	percent := (float64(numerator) / float64(denominator)) * 100
	if percent >= 10 {
		return fmt.Sprintf("%.0f%%", percent)
	}
	return fmt.Sprintf("%.1f%%", percent)
}

func liveTokenLines(innerW int, entries []agentTokens, summary tokenTotals, trendData map[string]usage.WorkspaceTrend) []string {
	lines := []string{tokenDimClr.Render(" live usage (MCP~ latest) ")}
	if len(entries) == 0 {
		return append(lines, tokenDimClr.Render("  (no token data)"))
	}

	header := fmt.Sprintf(" %-20s %9s %9s %9s %9s", "WORKSPACE", "INPUT", "OUTPUT", "COST", "MCP~")
	lines = append(lines, tokenDimClr.Render(header))

	maxCost := 0.0
	totalMCP := int64(0)
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
		latestMCP := int64(0)
		if trend, ok := trendData[e.Workspace]; ok {
			latestMCP = trend.LatestMCPProxy.Total
		}
		totalMCP += latestMCP

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
		mcpStr := formatMCPProxyMetric(latestMCP)

		ws := e.Workspace
		if len(ws) > 20 {
			ws = ws[:19] + "…"
		}
		line := fmt.Sprintf(" %-20s %9s %9s %9s %9s", ws, upStr, downStr, costStr, mcpStr)
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
	totalLine := fmt.Sprintf(" %-20s %9s %9s %9s %9s",
		fmt.Sprintf("TOTAL (%d)", summary.ReportingAgents),
		totalIn,
		totalOut,
		func() string {
			if summary.CostAgents == 0 {
				return "-"
			}
			return fmt.Sprintf("$%.2f", summary.TotalCost)
		}(),
		formatMCPProxyMetric(totalMCP))
	lines = append(lines, tokenSumClr.Render(totalLine))
	return lines
}

func trendTokenLines(innerW int, sessions []tmux.SessionInfo, trendData map[string]usage.WorkspaceTrend, workspaceInfos map[string]types.WorkspaceInfo) []string {
	lines := []string{tokenDimClr.Render(" history 24h (MCP~ proxy; offline retained) ")}
	seen := make(map[string]struct{}, len(sessions)+len(trendData))
	workspaceOrder := make([]string, 0, len(sessions)+len(trendData))
	for _, session := range sessions {
		if _, ok := seen[session.Workspace]; ok {
			continue
		}
		seen[session.Workspace] = struct{}{}
		workspaceOrder = append(workspaceOrder, session.Workspace)
	}
	for workspace := range trendData {
		if _, ok := seen[workspace]; ok {
			continue
		}
		seen[workspace] = struct{}{}
		workspaceOrder = append(workspaceOrder, workspace)
	}

	type trendRow struct {
		workspace string
		state     string
		last      int64
		total     int64
		mcp       int64
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
			state:     truncateStr(workspaceAgentStatus(workspaceInfos, workspace), 8),
			last:      trend.LatestTokens.Total(),
			total:     trend.Total.Total(),
			mcp:       trend.MCPProxy.Total,
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

	header := fmt.Sprintf(" %-15s %8s %8s %8s %8s %6s %-8s", "WORKSPACE", "STATE", "LAST", "24H", "MCP~", "SHARE", "TREND")
	lines = append(lines, tokenDimClr.Render(header))
	for _, row := range rows {
		ws := row.workspace
		if len(ws) > 15 {
			ws = ws[:14] + "…"
		}
		line := fmt.Sprintf(" %-15s %8s %8s %8s %8s %6s %-8s",
			ws,
			row.state,
			formatTokenCount(float64(row.last)),
			formatTokenCount(float64(row.total)),
			formatMCPProxyMetric(row.mcp),
			formatPercent(row.mcp, row.total),
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

func composeTokenLines(innerH int, liveLines, trendLines []string) []string {
	if innerH <= 0 {
		return nil
	}
	if len(trendLines) == 0 {
		return append([]string(nil), liveLines[:min(len(liveLines), innerH)]...)
	}

	total := len(liveLines) + len(trendLines)
	if total <= innerH {
		lines := make([]string, 0, total)
		lines = append(lines, liveLines...)
		lines = append(lines, trendLines...)
		return lines
	}

	liveBudget, trendBudget := splitTokenSectionBudget(innerH, true)
	liveTake := min(len(liveLines), liveBudget)
	trendTake := min(len(trendLines), trendBudget)
	remaining := innerH - liveTake - trendTake

	if remaining > 0 {
		extraLive := min(remaining, len(liveLines)-liveTake)
		liveTake += extraLive
		remaining -= extraLive
	}
	if remaining > 0 {
		extraTrend := min(remaining, len(trendLines)-trendTake)
		trendTake += extraTrend
	}

	lines := make([]string, 0, liveTake+trendTake)
	lines = append(lines, liveLines[:liveTake]...)
	lines = append(lines, trendLines[:trendTake]...)
	return lines
}

// renderTokens renders the optional detailed usage pane. Rows that only have a
// Claude done-line total use the output column to show Σ total tokens.
func (m watchModel) renderTokens(totalW, totalH int) string {
	innerW := totalW - 2
	innerH := totalH - 2
	if innerH < 1 {
		innerH = 1
	}

	title := tokenTitleSty.Render(" tokens ")
	topLine := renderPanelTopBorder(tokenBorderClr, title, innerW, watchTokenPanelHelpCandidates(m.streamOnly)...)

	entries := tokenEntriesFromMap(m.tokenData)
	summary := summarizeTokenEntries(entries, len(m.sessions))
	liveLines := liveTokenLines(innerW, entries, summary, m.trendData)
	trendLines := trendTokenLines(innerW, m.sessions, m.trendData, m.workspaceInfos)
	tokenLines := composeTokenLines(innerH, liveLines, trendLines)

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

func readHistoryFileUncached(path string, maxEntries int) []daemon.HistoryEntry {
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

func readTasksFileUncached(path string) []types.Task {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var tasks []types.Task
	if json.Unmarshal(data, &tasks) != nil {
		return nil
	}
	sortTasksForDisplay(tasks)
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
	return readTasksFileUncached(path), modTime
}

func taskSortOrder(s types.TaskStatus) int {
	switch s {
	case types.TaskInProgress:
		return 0
	case types.TaskPending:
		return 1
	case types.TaskFailed:
		return 2
	case types.TaskCancelled:
		return 3
	case types.TaskCompleted:
		return 4
	default:
		return 5
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
	case types.TaskCancelled:
		return taskFailClr
	default:
		return taskPendingClr
	}
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
func renderTaskSplitTopBorder(title string, listW, detailW int, helpCandidates ...string) string {
	leftSegmentW := listW + 1
	rightSegmentW := detailW + 1
	titleSegment := fitDisplayText(title, max(0, leftSegmentW-1))
	help := renderPanelHelpSegment(leftSegmentW-lipgloss.Width(titleSegment)-2, helpCandidates...)
	leftFill := leftSegmentW - lipgloss.Width(help) - lipgloss.Width(titleSegment) - 1
	if leftFill < 0 {
		help = ""
		leftFill = leftSegmentW - lipgloss.Width(titleSegment) - 1
	}
	if leftFill < 0 {
		leftFill = 0
	}
	return taskBorderClr.Render("╭─") +
		titleSegment +
		taskBorderClr.Render(strings.Repeat("─", leftFill)) +
		help +
		taskBorderClr.Render("┬"+strings.Repeat("─", rightSegmentW)+"╮")
}

// renderTaskSplitBottomBorder mirrors renderTaskSplitTopBorder so the divider
// column stays aligned across the full split view.
func renderTaskSplitBottomBorder(listW, detailW int) string {
	leftSegmentW := listW + 1
	rightSegmentW := detailW + 1
	return taskBorderClr.Render("╰" + strings.Repeat("─", leftSegmentW) + "┴" + strings.Repeat("─", rightSegmentW) + "╯")
}

// renderTasks renders the filtered task list and the currently selected task's
// detail pane in a connected split layout.
func (m watchModel) renderTasks(totalW, totalH int) string {
	innerW := totalW - 2
	innerH := totalH - 2
	if innerH < 1 {
		innerH = 1
	}

	filtered := filterTasksCached(m.tasks, m.taskFilter, tasksCacheVersionFor(m.tasksPath))
	title := taskTitleStyle.Render(fmt.Sprintf(" tasks %s %d/%d ", m.taskFilter.label(), len(filtered), len(m.tasks)))

	var bodyLines []string
	var topLine string
	var botLine string
	if len(filtered) == 0 {
		topLine = renderPanelTopBorder(taskBorderClr, title, innerW, watchTaskPanelHelpCandidates(m.streamOnly)...)
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
		topLine = renderTaskSplitTopBorder(title, listW, detailW, watchTaskPanelHelpCandidates(m.streamOnly)...)
		botLine = renderTaskSplitBottomBorder(listW, detailW)

		selectedIdx := clampIndex(m.taskSelected, len(filtered))
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
		fmt.Sprintf("version: %d", task.Version),
		fmt.Sprintf("assignee: %s", task.Assignee),
		fmt.Sprintf("created_by: %s", task.CreatedBy),
		fmt.Sprintf("priority: %s", taskPriorityLabel(task.Priority)),
		fmt.Sprintf("updated: %s ago", formatTaskAge(task)),
		fmt.Sprintf("start_mode: %s", task.StartMode),
		fmt.Sprintf("stale: %s", stale),
	)
	if task.RemovedAt != nil {
		lines = append(lines, fmt.Sprintf("removed: %s", task.RemovedAt.Format("2006-01-02 15:04:05")))
		if task.RemovedBy != "" {
			lines = append(lines, "removed_by: "+truncateStr(task.RemovedBy, width-12))
		}
	}
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
		if task.StaleInfo.WakePending {
			lines = append(lines, fmt.Sprintf("  wake_attempts: %d", task.StaleInfo.WakeAttempts))
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
