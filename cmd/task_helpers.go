package cmd

import (
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/types"
)

type taskActivityKind int

const (
	taskActivityLifecycle taskActivityKind = iota
	taskActivityLog
	taskActivityMessage
)

type taskActivityEntry struct {
	Timestamp time.Time
	Kind      taskActivityKind
	Actor     string
	Summary   string
	Detail    string
}

type taskSummary struct {
	Total           int
	Pending         int
	InProgress      int
	Completed       int
	Failed          int
	Cancelled       int
	Stale           int
	Diverged        int
	QueuedMessages  int
	UrgentOrHigh    int
	Recoverable     int
	TopAttentionIDs []string
}

type taskFilterMode int

const (
	taskFilterActive taskFilterMode = iota
	taskFilterStale
	taskFilterDone
	taskFilterAll
)

const taskListEntryHeight = 2

type taskListViewport struct {
	Start   int
	End     int
	Visible int
}

func (m taskFilterMode) label() string {
	switch m {
	case taskFilterActive:
		return "active"
	case taskFilterStale:
		return "stale"
	case taskFilterDone:
		return "done"
	case taskFilterAll:
		return "all"
	default:
		return "active"
	}
}

func nextTaskFilterMode(current taskFilterMode) taskFilterMode {
	return (current + 1) % 4
}

func filterTasks(tasks []types.Task, filter taskFilterMode) []types.Task {
	filtered := make([]types.Task, 0, len(tasks))
	for _, task := range tasks {
		switch filter {
		case taskFilterActive:
			if task.Status != types.TaskPending && task.Status != types.TaskInProgress {
				continue
			}
		case taskFilterStale:
			if !taskIsStale(task) {
				continue
			}
		case taskFilterDone:
			if task.Status != types.TaskCompleted && task.Status != types.TaskFailed && task.Status != types.TaskCancelled {
				continue
			}
		case taskFilterAll:
		}
		filtered = append(filtered, task)
	}
	sort.Slice(filtered, func(i, j int) bool {
		oi := taskSortOrder(filtered[i].Status)
		oj := taskSortOrder(filtered[j].Status)
		if oi != oj {
			return oi < oj
		}
		pi := taskPriorityOrder(filtered[i].Priority)
		pj := taskPriorityOrder(filtered[j].Priority)
		if pi != pj {
			return pi < pj
		}
		return filtered[i].UpdatedAt.After(filtered[j].UpdatedAt)
	})
	return filtered
}

func clampTaskSelection(current int, tasks []types.Task, filter taskFilterMode) int {
	return clampIndex(current, len(filterTasks(tasks, filter)))
}

// clampIndex clamps current into [0, n) and returns 0 when n is 0. It lets
// callers that already have a filtered slice avoid re-running filterTasks
// just to re-clamp.
func clampIndex(current, n int) int {
	if n <= 0 {
		return 0
	}
	if current < 0 {
		return 0
	}
	if current >= n {
		return n - 1
	}
	return current
}

func moveTaskSelection(current int, tasks []types.Task, filter taskFilterMode, delta int) int {
	filtered := filterTasks(tasks, filter)
	if len(filtered) == 0 {
		return 0
	}
	current += delta
	if current < 0 {
		current = 0
	}
	if current >= len(filtered) {
		current = len(filtered) - 1
	}
	return current
}

func taskListVisibleEntries(height int) int {
	visible := height / taskListEntryHeight
	if visible < 1 {
		return 1
	}
	return visible
}

func computeTaskListViewport(totalItems, selected, height int) taskListViewport {
	visible := taskListVisibleEntries(height)
	if totalItems <= 0 {
		return taskListViewport{Visible: visible}
	}
	if visible > totalItems {
		visible = totalItems
	}
	if selected < 0 {
		selected = 0
	}
	if selected >= totalItems {
		selected = totalItems - 1
	}
	start := 0
	if selected >= visible {
		start = selected - visible + 1
	}
	maxStart := totalItems - visible
	if start > maxStart {
		start = maxStart
	}
	return taskListViewport{
		Start:   start,
		End:     start + visible,
		Visible: visible,
	}
}

func selectedTask(tasks []types.Task, filter taskFilterMode, index int) *types.Task {
	filtered := filterTasks(tasks, filter)
	if len(filtered) == 0 {
		return nil
	}
	if index < 0 || index >= len(filtered) {
		index = 0
	}
	task := filtered[index]
	return &task
}

func taskIsStale(task types.Task) bool {
	if task.StaleInfo != nil && task.StaleInfo.IsStale {
		return true
	}
	if task.Status != types.TaskPending && task.Status != types.TaskInProgress {
		return false
	}
	if task.StaleAfterSeconds <= 0 {
		return false
	}
	return int(time.Since(task.UpdatedAt).Seconds()) >= task.StaleAfterSeconds
}

func taskAge(task types.Task) time.Duration {
	return time.Since(task.UpdatedAt).Round(time.Second)
}

func formatTaskAge(task types.Task) string {
	return formatAge(taskAge(task))
}

func formatAge(d time.Duration) string {
	if d < 0 {
		d = 0
	}
	switch {
	case d < time.Minute:
		return fmt.Sprintf("%ds", int(d.Seconds()))
	case d < time.Hour:
		return fmt.Sprintf("%dm", int(d.Minutes()))
	case d < 24*time.Hour:
		return fmt.Sprintf("%dh", int(d.Hours()))
	default:
		return fmt.Sprintf("%dd", int(d.Hours()/24))
	}
}

func taskStatusLabel(task types.Task) string {
	label := string(task.Status)
	if taskIsStale(task) && task.Status != types.TaskCompleted && task.Status != types.TaskFailed && task.Status != types.TaskCancelled {
		label += " stale"
	}
	return label
}

func taskPriorityOrder(priority types.TaskPriority) int {
	switch priority {
	case types.TaskPriorityUrgent:
		return 0
	case types.TaskPriorityHigh:
		return 1
	case types.TaskPriorityNormal, "":
		return 2
	case types.TaskPriorityLow:
		return 3
	default:
		return 4
	}
}

func taskPriorityLabel(priority types.TaskPriority) string {
	if priority == "" {
		return string(types.TaskPriorityNormal)
	}
	return string(priority)
}

func taskLastUpdatePreview(task types.Task) string {
	if len(task.Logs) > 0 {
		return task.Logs[len(task.Logs)-1].Message
	}
	if task.Result != "" {
		return task.Result
	}
	if task.Description != "" {
		return task.Description
	}
	return ""
}

func recentTaskLogs(task types.Task, limit int) []types.TaskLog {
	if limit <= 0 || len(task.Logs) <= limit {
		return task.Logs
	}
	return task.Logs[len(task.Logs)-limit:]
}

func relatedTaskMessages(task types.Task, history []daemon.HistoryEntry, limit int) []daemon.HistoryEntry {
	if limit <= 0 {
		return nil
	}
	var related []daemon.HistoryEntry
	for i := len(history) - 1; i >= 0; i-- {
		entry := history[i]
		if entry.TaskID == task.ID ||
			strings.Contains(entry.Content, task.ID) ||
			entry.From == task.Assignee ||
			entry.To == task.Assignee ||
			entry.From == task.CreatedBy ||
			entry.To == task.CreatedBy {
			related = append(related, entry)
			if len(related) == limit {
				break
			}
		}
	}
	for i, j := 0, len(related)-1; i < j; i, j = i+1, j-1 {
		related[i], related[j] = related[j], related[i]
	}
	return related
}

func buildTaskActivity(task types.Task, history []daemon.HistoryEntry, limit int) []taskActivityEntry {
	entries := []taskActivityEntry{
		{
			Timestamp: task.CreatedAt,
			Kind:      taskActivityLifecycle,
			Actor:     task.CreatedBy,
			Summary:   fmt.Sprintf("created task for %s", task.Assignee),
			Detail:    task.Description,
		},
	}

	if task.Status == types.TaskCompleted && task.Result != "" {
		entries = append(entries, taskActivityEntry{
			Timestamp: task.UpdatedAt,
			Kind:      taskActivityLifecycle,
			Actor:     task.Assignee,
			Summary:   "completed task",
			Detail:    task.Result,
		})
	} else if task.Status == types.TaskFailed && task.Result != "" {
		entries = append(entries, taskActivityEntry{
			Timestamp: task.UpdatedAt,
			Kind:      taskActivityLifecycle,
			Actor:     task.Assignee,
			Summary:   "failed task",
			Detail:    task.Result,
		})
	} else if task.Status == types.TaskCancelled && task.Result != "" {
		entries = append(entries, taskActivityEntry{
			Timestamp: task.UpdatedAt,
			Kind:      taskActivityLifecycle,
			Actor:     task.Assignee,
			Summary:   "cancelled task",
			Detail:    task.Result,
		})
	} else if task.Status == types.TaskInProgress {
		entries = append(entries, taskActivityEntry{
			Timestamp: task.UpdatedAt,
			Kind:      taskActivityLifecycle,
			Actor:     task.Assignee,
			Summary:   "task in progress",
		})
	}
	if task.RemovedAt != nil {
		removedBy := task.RemovedBy
		if removedBy == "" {
			removedBy = task.CreatedBy
		}
		entries = append(entries, taskActivityEntry{
			Timestamp: *task.RemovedAt,
			Kind:      taskActivityLifecycle,
			Actor:     removedBy,
			Summary:   "removed task",
			Detail:    task.RemoveReason,
		})
	}

	for _, log := range task.Logs {
		entries = append(entries, taskActivityEntry{
			Timestamp: log.Timestamp,
			Kind:      taskActivityLog,
			Actor:     log.Workspace,
			Summary:   log.Message,
		})
	}

	for _, msg := range relatedTaskMessages(task, history, 0) {
		summary := strings.ReplaceAll(msg.Content, "\n", " ")
		if strings.Contains(summary, task.ID) {
			summary = strings.ReplaceAll(summary, task.ID, shortTaskID(task.ID))
		}
		entries = append(entries, taskActivityEntry{
			Timestamp: msg.Timestamp,
			Kind:      taskActivityMessage,
			Actor:     msg.From + "->" + msg.To,
			Summary:   summary,
		})
	}

	sort.Slice(entries, func(i, j int) bool {
		if entries[i].Timestamp.Equal(entries[j].Timestamp) {
			return entries[i].Kind < entries[j].Kind
		}
		return entries[i].Timestamp.Before(entries[j].Timestamp)
	})

	if limit > 0 && len(entries) > limit {
		entries = entries[len(entries)-limit:]
	}
	return entries
}

func shortTaskID(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
}

func summarizeTasks(tasks []types.Task) taskSummary {
	summary := taskSummary{Total: len(tasks)}
	var topAttention []types.Task
	for _, task := range tasks {
		switch task.Status {
		case types.TaskPending:
			summary.Pending++
		case types.TaskInProgress:
			summary.InProgress++
		case types.TaskCompleted:
			summary.Completed++
		case types.TaskFailed:
			summary.Failed++
		case types.TaskCancelled:
			summary.Cancelled++
		}

		if task.Priority == types.TaskPriorityUrgent || task.Priority == types.TaskPriorityHigh {
			summary.UrgentOrHigh++
		}
		if taskIsStale(task) {
			summary.Stale++
		}
		if task.StaleInfo != nil {
			summary.QueuedMessages += task.StaleInfo.PendingMessages
			if task.StaleInfo.StateDivergence {
				summary.Diverged++
			}
			if task.StaleInfo.IsStale || task.StaleInfo.StateDivergence {
				summary.Recoverable++
				topAttention = append(topAttention, task)
			}
		}
	}

	sort.Slice(topAttention, func(i, j int) bool {
		pi := taskPriorityOrder(topAttention[i].Priority)
		pj := taskPriorityOrder(topAttention[j].Priority)
		if pi != pj {
			return pi < pj
		}
		if taskIsStale(topAttention[i]) != taskIsStale(topAttention[j]) {
			return taskIsStale(topAttention[i])
		}
		return topAttention[i].UpdatedAt.Before(topAttention[j].UpdatedAt)
	})
	for i := 0; i < len(topAttention) && i < 3; i++ {
		summary.TopAttentionIDs = append(summary.TopAttentionIDs, shortTaskID(topAttention[i].ID))
	}

	return summary
}

func taskOperatorHint(task types.Task) string {
	if task.StaleInfo != nil {
		switch {
		case task.StaleInfo.IsStale && task.StaleInfo.RecommendedAction != "":
			return task.StaleInfo.RecommendedAction
		case task.StaleInfo.StateDivergence:
			return task.StaleInfo.StateDivergenceNote
		case task.StaleInfo.PendingMessages > 0:
			return fmt.Sprintf("%d pending message(s) queued", task.StaleInfo.PendingMessages)
		}
	}
	if preview := taskLastUpdatePreview(task); preview != "" {
		return preview
	}
	return "awaiting next progress update"
}

func formatTaskSummary(summary taskSummary) string {
	parts := []string{
		fmt.Sprintf("total=%d", summary.Total),
		fmt.Sprintf("pending=%d", summary.Pending),
		fmt.Sprintf("in_progress=%d", summary.InProgress),
		fmt.Sprintf("stale=%d", summary.Stale),
		fmt.Sprintf("diverged=%d", summary.Diverged),
		fmt.Sprintf("queued_msgs=%d", summary.QueuedMessages),
	}
	if summary.Cancelled > 0 {
		parts = append(parts, fmt.Sprintf("cancelled=%d", summary.Cancelled))
	}
	if summary.UrgentOrHigh > 0 {
		parts = append(parts, fmt.Sprintf("high_pri=%d", summary.UrgentOrHigh))
	}
	if len(summary.TopAttentionIDs) > 0 {
		parts = append(parts, "attention="+strings.Join(summary.TopAttentionIDs, ","))
	}
	return strings.Join(parts, "  ")
}

func taskExpectedVersionArg(task types.Task) string {
	if task.Version <= 0 {
		return ""
	}
	return fmt.Sprintf(" --expected-version %d", task.Version)
}

func taskRecoveryPreviewLines(task types.Task) []string {
	lines := []string{
		fmt.Sprintf("Task: %s", task.Title),
		fmt.Sprintf("ID: %s", task.ID),
		fmt.Sprintf("Status: %s", taskStatusLabel(task)),
		fmt.Sprintf("Version: %d", task.Version),
		fmt.Sprintf("Assignee: %s", task.Assignee),
		fmt.Sprintf("Created By: %s", task.CreatedBy),
		fmt.Sprintf("Updated: %s ago", formatTaskAge(task)),
	}
	if task.RemovedAt != nil {
		lines = append(lines, fmt.Sprintf("Removed: %s by %s", task.RemovedAt.Format("2006-01-02 15:04:05"), truncateStr(task.RemovedBy, 24)))
		if task.RemoveReason != "" {
			lines = append(lines, "Remove Reason: "+task.RemoveReason)
		}
		lines = append(lines, "", "Semantics:", "- recover is preview-only and this task is already archived/removed from list results")
		return lines
	}
	if task.StaleInfo != nil {
		lines = append(lines, "", "Signals:")
		if task.StaleInfo.Reason != "" {
			lines = append(lines, "- reason: "+task.StaleInfo.Reason)
		}
		if task.StaleInfo.RecommendedAction != "" {
			lines = append(lines, "- daemon hint: "+task.StaleInfo.RecommendedAction)
		}
		if task.StaleInfo.PendingMessages > 0 {
			lines = append(lines, fmt.Sprintf("- pending_messages: %d", task.StaleInfo.PendingMessages))
		}
		if task.StaleInfo.WakePending {
			wake := fmt.Sprintf("- wake_retry: attempt %d pending", task.StaleInfo.WakeAttempts)
			if task.StaleInfo.NextWakeRetryAt != nil {
				wake += " until " + task.StaleInfo.NextWakeRetryAt.Format("2006-01-02 15:04:05")
			}
			lines = append(lines, wake)
		}
		if task.StaleInfo.StateDivergence {
			lines = append(lines, "- divergence: "+task.StaleInfo.StateDivergenceNote)
		}
	}

	lines = append(lines, "", "Semantics:", "- recover is preview-only; use intervene/retry/cancel/remove to mutate the task")
	versionArg := taskExpectedVersionArg(task)
	if task.Status == types.TaskPending || task.Status == types.TaskInProgress {
		lines = append(lines,
			"",
			"Next steps:",
			fmt.Sprintf("- ax tasks intervene %s --action wake%s", task.ID, versionArg),
			fmt.Sprintf("- ax tasks intervene %s --action interrupt%s", task.ID, versionArg),
			fmt.Sprintf("- ax tasks retry %s%s", task.ID, versionArg),
			"  retry queues a standardized follow-up message on the same task ID",
			fmt.Sprintf("- ax tasks cancel %s%s", task.ID, versionArg),
		)
		return lines
	}

	lines = append(lines, "", "Next steps:")
	lines = append(lines, "- task is terminal; intervene/retry is unavailable")
	if task.Status == types.TaskCompleted || task.Status == types.TaskFailed || task.Status == types.TaskCancelled {
		lines = append(lines, fmt.Sprintf("- ax tasks remove %s%s", task.ID, versionArg))
	}
	return lines
}
