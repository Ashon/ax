package cmd

import (
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/mcpserver"
	"github.com/ashon/ax/internal/types"
	"github.com/spf13/cobra"
)

var (
	taskAssignee      string
	taskCreatedBy     string
	taskStatus        string
	taskOnlyStale     bool
	taskLogLimit      int
	taskActivityLimit int

	taskCancelReason          string
	taskCancelExpectedVersion int64
	taskRemoveReason          string
	taskRemoveExpectedVersion int64
	taskInterveneAction       string
	taskInterveneNote         string
	taskInterveneExpectedVer  int64
	taskRetryNote             string
	taskRetryExpectedVersion  int64
)

var tasksCmd = &cobra.Command{
	Use:   "tasks",
	Short: "Inspect and control task status",
	RunE: func(cmd *cobra.Command, args []string) error {
		client, err := newCLIClient()
		if err != nil {
			return err
		}
		defer client.Close()

		status, err := parseTaskStatusFlag(taskStatus)
		if err != nil {
			return err
		}
		tasks, err := client.ListTasks(taskAssignee, taskCreatedBy, status)
		if err != nil {
			return err
		}
		if taskOnlyStale {
			tasks = filterTasks(tasks, taskFilterStale)
		} else {
			sort.Slice(tasks, func(i, j int) bool {
				oi := taskSortOrder(tasks[i].Status)
				oj := taskSortOrder(tasks[j].Status)
				if oi != oj {
					return oi < oj
				}
				pi := taskPriorityOrder(tasks[i].Priority)
				pj := taskPriorityOrder(tasks[j].Priority)
				if pi != pj {
					return pi < pj
				}
				return tasks[i].UpdatedAt.After(tasks[j].UpdatedAt)
			})
		}
		if len(tasks) == 0 {
			fmt.Println("No tasks found.")
			return nil
		}

		fmt.Printf("Summary: %s\n\n", formatTaskSummary(summarizeTasks(tasks)))
		fmt.Printf("%-8s %-8s %-18s %-6s %-16s %-16s %-24s %s\n", "ID", "PRI", "STATUS", "AGE", "ASSIGNEE", "CREATED BY", "TITLE", "NEXT SIGNAL")
		for _, task := range tasks {
			id := task.ID
			if len(id) > 8 {
				id = id[:8]
			}
			fmt.Printf("%-8s %-8s %-18s %-6s %-16s %-16s %-24s %s\n",
				id,
				truncateStr(taskPriorityLabel(task.Priority), 8),
				truncateStr(taskStatusLabel(task), 18),
				formatTaskAge(task),
				truncateStr(task.Assignee, 16),
				truncateStr(task.CreatedBy, 16),
				truncateStr(task.Title, 24),
				truncateStr(strings.ReplaceAll(taskOperatorHint(task), "\n", " "), 72),
			)
		}
		return nil
	},
}

var tasksShowCmd = &cobra.Command{
	Use:   "show <task-id>",
	Short: "Show task details, recent logs, and related messages",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		client, err := newCLIClient()
		if err != nil {
			return err
		}
		defer client.Close()

		task, err := client.GetTask(args[0])
		if err != nil {
			return err
		}

		fmt.Printf("Task: %s\n", task.Title)
		fmt.Printf("ID: %s\n", task.ID)
		fmt.Printf("Status: %s\n", taskStatusLabel(*task))
		fmt.Printf("Version: %d\n", task.Version)
		fmt.Printf("Assignee: %s\n", task.Assignee)
		fmt.Printf("Created By: %s\n", task.CreatedBy)
		fmt.Printf("Priority: %s\n", taskPriorityLabel(task.Priority))
		fmt.Printf("Updated: %s ago (%s)\n", formatTaskAge(*task), task.UpdatedAt.Format("2006-01-02 15:04:05"))
		fmt.Printf("Created: %s\n", task.CreatedAt.Format("2006-01-02 15:04:05"))
		fmt.Printf("Start Mode: %s\n", task.StartMode)
		if task.RemovedAt != nil {
			fmt.Printf("Removed: %s\n", task.RemovedAt.Format("2006-01-02 15:04:05"))
			if task.RemovedBy != "" {
				fmt.Printf("Removed By: %s\n", task.RemovedBy)
			}
			if task.RemoveReason != "" {
				fmt.Printf("Remove Reason: %s\n", task.RemoveReason)
			}
		}
		if task.StaleAfterSeconds > 0 {
			fmt.Printf("Stale After: %ds\n", task.StaleAfterSeconds)
		}
		if task.Description != "" {
			fmt.Printf("\nDescription:\n%s\n", task.Description)
		}
		if task.Result != "" {
			fmt.Printf("\nResult:\n%s\n", task.Result)
		}
		if task.StaleInfo != nil {
			fmt.Printf("\nStale Info:\n")
			fmt.Printf("- is_stale: %t\n", task.StaleInfo.IsStale)
			fmt.Printf("- age: %s\n", formatAge(time.Duration(task.StaleInfo.AgeSeconds)*time.Second))
			if task.StaleInfo.Reason != "" {
				fmt.Printf("- reason: %s\n", task.StaleInfo.Reason)
			}
			if task.StaleInfo.RecommendedAction != "" {
				fmt.Printf("- action: %s\n", task.StaleInfo.RecommendedAction)
			}
			if task.StaleInfo.PendingMessages > 0 {
				fmt.Printf("- pending_messages: %d\n", task.StaleInfo.PendingMessages)
			}
			if task.StaleInfo.StateDivergence {
				fmt.Printf("- divergence: %s\n", task.StaleInfo.StateDivergenceNote)
			}
			if task.StaleInfo.LastMessageAt != nil {
				fmt.Printf("- last_message: %s\n", task.StaleInfo.LastMessageAt.Format("2006-01-02 15:04:05"))
			}
			if task.StaleInfo.WakePending {
				fmt.Printf("- wake_pending: true\n")
				if task.StaleInfo.WakeAttempts > 0 {
					fmt.Printf("- wake_attempts: %d\n", task.StaleInfo.WakeAttempts)
				}
				if task.StaleInfo.NextWakeRetryAt != nil {
					fmt.Printf("- next_wake_retry: %s\n", task.StaleInfo.NextWakeRetryAt.Format("2006-01-02 15:04:05"))
				}
			}
		}

		fmt.Printf("\nOperator Hint:\n%s\n", taskOperatorHint(*task))

		fmt.Printf("\nRecent Logs:\n")
		logs := recentTaskLogs(*task, taskLogLimit)
		if len(logs) == 0 {
			fmt.Println("(none)")
		} else {
			for _, log := range logs {
				fmt.Printf("- %s %s: %s\n", log.Timestamp.Format("15:04:05"), log.Workspace, log.Message)
			}
		}

		fmt.Printf("\nRelated Messages:\n")
		history := readHistoryFile(daemon.HistoryFilePath(socketPath), 200)
		msgs := relatedTaskMessages(*task, history, 6)
		if len(msgs) == 0 {
			fmt.Println("(none)")
		} else {
			for _, msg := range msgs {
				content := strings.ReplaceAll(msg.Content, "\n", " ")
				fmt.Printf("- %s %s -> %s: %s\n", msg.Timestamp.Format("15:04:05"), msg.From, msg.To, truncateStr(content, 120))
			}
		}
		return nil
	},
}

var tasksCancelCmd = &cobra.Command{
	Use:   "cancel <task-id>",
	Short: "Cancel an active task",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		client, err := newCLIClient()
		if err != nil {
			return err
		}
		defer client.Close()

		task, err := client.CancelTask(args[0], strings.TrimSpace(taskCancelReason), optionalTaskVersion(taskCancelExpectedVersion))
		if err != nil {
			return err
		}
		printTaskMutationResult("Cancelled", task)
		return nil
	},
}

var tasksRemoveCmd = &cobra.Command{
	Use:   "remove <task-id>",
	Short: "Archive a terminal task so it disappears from list results",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		client, err := newCLIClient()
		if err != nil {
			return err
		}
		defer client.Close()

		task, err := client.RemoveTask(args[0], strings.TrimSpace(taskRemoveReason), optionalTaskVersion(taskRemoveExpectedVersion))
		if err != nil {
			return err
		}
		printTaskMutationResult("Removed", task)
		return nil
	},
}

var tasksRecoverCmd = &cobra.Command{
	Use:   "recover <task-id>",
	Short: "Preview safe next steps for a task without mutating it",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		client, err := newCLIClient()
		if err != nil {
			return err
		}
		defer client.Close()

		task, err := client.GetTask(args[0])
		if err != nil {
			return err
		}
		fmt.Println(strings.Join(taskRecoveryPreviewLines(*task), "\n"))
		return nil
	},
}

var tasksInterveneCmd = &cobra.Command{
	Use:   "intervene <task-id>",
	Short: "Apply a bounded recovery action to a task",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		client, err := newCLIClient()
		if err != nil {
			return err
		}
		defer client.Close()

		action := strings.TrimSpace(taskInterveneAction)
		if action != "wake" && action != "interrupt" && action != "retry" {
			return fmt.Errorf("invalid --action %q (must be wake, interrupt, or retry)", action)
		}

		resp, err := client.InterveneTask(args[0], action, strings.TrimSpace(taskInterveneNote), optionalTaskVersion(taskInterveneExpectedVer))
		if err != nil {
			return err
		}
		printTaskInterventionResult(resp)
		return nil
	},
}

var tasksRetryCmd = &cobra.Command{
	Use:   "retry <task-id>",
	Short: "Queue a standardized follow-up message on the same task ID",
	Args:  cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		client, err := newCLIClient()
		if err != nil {
			return err
		}
		defer client.Close()

		resp, err := client.InterveneTask(args[0], "retry", strings.TrimSpace(taskRetryNote), optionalTaskVersion(taskRetryExpectedVersion))
		if err != nil {
			return err
		}
		printTaskInterventionResult(resp)
		return nil
	},
}

var tasksActivityCmd = &cobra.Command{
	Use:   "activity [task-id]",
	Short: "Show chronological task activity across logs and related messages",
	Args:  cobra.MaximumNArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		client, err := newCLIClient()
		if err != nil {
			return err
		}
		defer client.Close()

		history := readHistoryFile(daemon.HistoryFilePath(socketPath), 500)
		if len(args) == 1 {
			task, err := client.GetTask(args[0])
			if err != nil {
				return err
			}
			printTaskActivity(*task, history, taskActivityLimit)
			return nil
		}

		status, err := parseTaskStatusFlag(taskStatus)
		if err != nil {
			return err
		}
		tasks, err := client.ListTasks(taskAssignee, taskCreatedBy, status)
		if err != nil {
			return err
		}
		if taskOnlyStale {
			tasks = filterTasks(tasks, taskFilterStale)
		}
		if len(tasks) == 0 {
			fmt.Println("No tasks found.")
			return nil
		}

		var entries []taskActivityEntry
		for _, task := range tasks {
			for _, entry := range buildTaskActivity(task, history, 0) {
				entry.Detail = shortTaskID(task.ID) + " " + task.Title
				entries = append(entries, entry)
			}
		}
		sort.Slice(entries, func(i, j int) bool {
			return entries[i].Timestamp.After(entries[j].Timestamp)
		})
		if taskActivityLimit > 0 && len(entries) > taskActivityLimit {
			entries = entries[:taskActivityLimit]
		}
		for _, entry := range entries {
			fmt.Printf("%s %-12s %-22s %s", entry.Timestamp.Format("2006-01-02 15:04:05"), activityKindLabel(entry.Kind), truncateStr(entry.Actor, 22), truncateStr(entry.Summary, 88))
			if entry.Detail != "" {
				fmt.Printf("  [%s]", truncateStr(entry.Detail, 48))
			}
			fmt.Println()
		}
		return nil
	},
}

func printTaskActivity(task types.Task, history []daemon.HistoryEntry, limit int) {
	fmt.Printf("Activity: %s (%s)\n\n", task.Title, task.ID)
	entries := buildTaskActivity(task, history, limit)
	if len(entries) == 0 {
		fmt.Println("(no activity)")
		return
	}
	for _, entry := range entries {
		fmt.Printf("%s %-12s %-22s %s\n", entry.Timestamp.Format("2006-01-02 15:04:05"), activityKindLabel(entry.Kind), truncateStr(entry.Actor, 22), entry.Summary)
		if entry.Detail != "" {
			fmt.Printf("  %s\n", truncateStr(strings.ReplaceAll(entry.Detail, "\n", " "), 140))
		}
	}
}

func activityKindLabel(kind taskActivityKind) string {
	switch kind {
	case taskActivityLifecycle:
		return "lifecycle"
	case taskActivityLog:
		return "log"
	case taskActivityMessage:
		return "message"
	default:
		return "activity"
	}
}

func newCLIClient() (*mcpserver.DaemonClient, error) {
	client := mcpserver.NewDaemonClient(socketPath, "_cli")
	if err := client.Connect(); err != nil {
		return nil, fmt.Errorf("connect to daemon: %w (is daemon running?)", err)
	}
	return client, nil
}

func parseTaskStatusFlag(raw string) (*types.TaskStatus, error) {
	if raw == "" {
		return nil, nil
	}
	status := types.TaskStatus(raw)
	switch status {
	case types.TaskPending, types.TaskInProgress, types.TaskCompleted, types.TaskFailed, types.TaskCancelled:
		return &status, nil
	default:
		return nil, fmt.Errorf("invalid --status %q", raw)
	}
}

func optionalTaskVersion(version int64) *int64 {
	if version <= 0 {
		return nil
	}
	v := version
	return &v
}

func printTaskMutationResult(action string, task *types.Task) {
	fmt.Printf("%s task %s\n", action, task.ID)
	fmt.Printf("Status: %s\n", taskStatusLabel(*task))
	fmt.Printf("Version: %d\n", task.Version)
	fmt.Printf("Assignee: %s\n", task.Assignee)
	if task.Result != "" {
		fmt.Printf("Result: %s\n", task.Result)
	}
	if task.RemovedAt != nil {
		fmt.Printf("Removed: %s\n", task.RemovedAt.Format("2006-01-02 15:04:05"))
		if task.RemovedBy != "" {
			fmt.Printf("Removed By: %s\n", task.RemovedBy)
		}
		if task.RemoveReason != "" {
			fmt.Printf("Remove Reason: %s\n", task.RemoveReason)
		}
	}
}

func printTaskInterventionResult(resp *daemon.InterveneTaskResponse) {
	fmt.Printf("Intervened task %s\n", resp.Task.ID)
	fmt.Printf("Action: %s\n", resp.Action)
	fmt.Printf("Status: %s\n", resp.Status)
	fmt.Printf("Task Status: %s\n", taskStatusLabel(resp.Task))
	fmt.Printf("Version: %d\n", resp.Task.Version)
	if resp.MessageID != "" {
		fmt.Printf("Message ID: %s\n", resp.MessageID)
	}
	if resp.Action == "retry" {
		fmt.Println("Retry semantics: queued a standardized follow-up message on the same task ID.")
	}
}

func init() {
	tasksCmd.Flags().StringVar(&taskAssignee, "assignee", "", "filter by assignee workspace")
	tasksCmd.Flags().StringVar(&taskCreatedBy, "created-by", "", "filter by creator workspace")
	tasksCmd.Flags().StringVar(&taskStatus, "status", "", "filter by status: pending|in_progress|completed|failed|cancelled")
	tasksCmd.Flags().BoolVar(&taskOnlyStale, "stale", false, "show only stale tasks")

	tasksShowCmd.Flags().IntVar(&taskLogLimit, "logs", 8, "number of recent logs to show")
	tasksActivityCmd.Flags().IntVar(&taskActivityLimit, "limit", 20, "number of activity entries to show")
	tasksCancelCmd.Flags().StringVar(&taskCancelReason, "reason", "", "optional cancellation reason")
	tasksCancelCmd.Flags().Int64Var(&taskCancelExpectedVersion, "expected-version", 0, "optional optimistic concurrency guard")
	tasksRemoveCmd.Flags().StringVar(&taskRemoveReason, "reason", "", "optional archive reason")
	tasksRemoveCmd.Flags().Int64Var(&taskRemoveExpectedVersion, "expected-version", 0, "optional optimistic concurrency guard")
	tasksInterveneCmd.Flags().StringVar(&taskInterveneAction, "action", "", "bounded action: wake, interrupt, or retry")
	tasksInterveneCmd.Flags().StringVar(&taskInterveneNote, "note", "", "optional note for retry follow-up messages")
	tasksInterveneCmd.Flags().Int64Var(&taskInterveneExpectedVer, "expected-version", 0, "optional optimistic concurrency guard")
	tasksRetryCmd.Flags().StringVar(&taskRetryNote, "note", "", "optional note for the queued follow-up message")
	tasksRetryCmd.Flags().Int64Var(&taskRetryExpectedVersion, "expected-version", 0, "optional optimistic concurrency guard")

	tasksCmd.AddCommand(tasksShowCmd, tasksActivityCmd, tasksCancelCmd, tasksRemoveCmd, tasksRecoverCmd, tasksInterveneCmd, tasksRetryCmd)
	rootCmd.AddCommand(tasksCmd)
}
