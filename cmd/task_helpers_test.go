package cmd

import (
	"strings"
	"testing"
	"time"

	"github.com/ashon/ax/internal/types"
)

func TestSummarizeTasksCapturesOperatorSignals(t *testing.T) {
	now := time.Now()
	tasks := []types.Task{
		{
			ID:        "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
			Status:    types.TaskPending,
			Priority:  types.TaskPriorityHigh,
			UpdatedAt: now.Add(-10 * time.Minute),
			StaleInfo: &types.TaskStaleInfo{
				IsStale:           true,
				PendingMessages:   2,
				StateDivergence:   true,
				RecommendedAction: "redispatch",
			},
		},
		{
			ID:        "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
			Status:    types.TaskInProgress,
			Priority:  types.TaskPriorityNormal,
			UpdatedAt: now.Add(-2 * time.Minute),
			StaleInfo: &types.TaskStaleInfo{
				PendingMessages: 1,
			},
		},
		{
			ID:        "cccccccc-cccc-cccc-cccc-cccccccccccc",
			Status:    types.TaskCompleted,
			Priority:  types.TaskPriorityLow,
			UpdatedAt: now,
		},
	}

	summary := summarizeTasks(tasks)
	if summary.Total != 3 || summary.Pending != 1 || summary.InProgress != 1 || summary.Completed != 1 {
		t.Fatalf("unexpected task counts: %+v", summary)
	}
	if summary.Stale != 1 || summary.Diverged != 1 || summary.QueuedMessages != 3 {
		t.Fatalf("unexpected operator signals: %+v", summary)
	}
	if summary.UrgentOrHigh != 1 || summary.Recoverable != 1 {
		t.Fatalf("unexpected priority/recoverable counts: %+v", summary)
	}
	if len(summary.TopAttentionIDs) == 0 || summary.TopAttentionIDs[0] != "aaaaaaaa" {
		t.Fatalf("expected top attention task to be surfaced, got %+v", summary.TopAttentionIDs)
	}
}

func TestTaskOperatorHintPrefersRecoverySignal(t *testing.T) {
	task := types.Task{
		Description: "fallback description",
		StaleInfo: &types.TaskStaleInfo{
			IsStale:           true,
			RecommendedAction: "inspect workspace and redispatch",
		},
	}
	if got := taskOperatorHint(task); got != "inspect workspace and redispatch" {
		t.Fatalf("expected recommended action, got %q", got)
	}

	task.StaleInfo = &types.TaskStaleInfo{StateDivergence: true, StateDivergenceNote: "pending/in_progress mismatch"}
	if got := taskOperatorHint(task); got != "pending/in_progress mismatch" {
		t.Fatalf("expected divergence note, got %q", got)
	}

	task.StaleInfo = &types.TaskStaleInfo{PendingMessages: 2}
	if got := taskOperatorHint(task); got != "2 pending message(s) queued" {
		t.Fatalf("expected pending message hint, got %q", got)
	}
}

func TestSortTasksForDisplayUsesDeterministicTieBreakers(t *testing.T) {
	baseUpdated := time.Unix(2000, 0)
	olderCreated := time.Unix(1500, 0)
	newerCreated := time.Unix(1600, 0)

	tasks := []types.Task{
		{
			ID:        "ccc",
			Status:    types.TaskPending,
			Priority:  types.TaskPriorityHigh,
			UpdatedAt: baseUpdated,
			CreatedAt: olderCreated,
		},
		{
			ID:        "aaa",
			Status:    types.TaskPending,
			Priority:  types.TaskPriorityHigh,
			UpdatedAt: baseUpdated,
			CreatedAt: olderCreated,
		},
		{
			ID:        "bbb",
			Status:    types.TaskPending,
			Priority:  types.TaskPriorityHigh,
			UpdatedAt: baseUpdated,
			CreatedAt: newerCreated,
		},
	}

	sortTasksForDisplay(tasks)

	got := []string{tasks[0].ID, tasks[1].ID, tasks[2].ID}
	want := []string{"bbb", "aaa", "ccc"}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("sorted IDs = %v, want %v", got, want)
		}
	}
}

func TestFormatTaskSummaryIncludesAttentionHints(t *testing.T) {
	text := formatTaskSummary(taskSummary{
		Total:          4,
		Pending:        1,
		InProgress:     2,
		Cancelled:      1,
		Stale:          1,
		Diverged:       1,
		QueuedMessages: 3,
		UrgentOrHigh:   2,
		TopAttentionIDs: []string{
			"deadbeef",
			"cafebabe",
		},
	})

	for _, want := range []string{
		"total=4",
		"pending=1",
		"in_progress=2",
		"cancelled=1",
		"stale=1",
		"diverged=1",
		"queued_msgs=3",
		"high_pri=2",
		"attention=deadbeef,cafebabe",
	} {
		if !strings.Contains(text, want) {
			t.Fatalf("expected %q in summary %q", want, text)
		}
	}
}

func TestTaskRecoveryPreviewLinesForActiveTask(t *testing.T) {
	task := types.Task{
		ID:        "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
		Title:     "Recover worker",
		Status:    types.TaskInProgress,
		Version:   7,
		Assignee:  "ax.worker",
		CreatedBy: "ax.orchestrator",
		UpdatedAt: time.Now().Add(-5 * time.Minute),
		StaleInfo: &types.TaskStaleInfo{
			Reason:            "no task progress update",
			RecommendedAction: "inspect the assignee workspace",
			PendingMessages:   2,
		},
	}

	text := strings.Join(taskRecoveryPreviewLines(task), "\n")
	for _, want := range []string{
		"recover is preview-only",
		"ax tasks intervene aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa --action wake --expected-version 7",
		"ax tasks retry aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa --expected-version 7",
		"same task ID",
		"ax tasks cancel aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa --expected-version 7",
	} {
		if !strings.Contains(text, want) {
			t.Fatalf("expected %q in recovery preview %q", want, text)
		}
	}
}

func TestTaskRecoveryPreviewLinesForTerminalTask(t *testing.T) {
	task := types.Task{
		ID:        "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
		Title:     "Finished task",
		Status:    types.TaskCancelled,
		Version:   3,
		Assignee:  "ax.worker",
		CreatedBy: "ax.orchestrator",
		UpdatedAt: time.Now(),
	}

	text := strings.Join(taskRecoveryPreviewLines(task), "\n")
	for _, want := range []string{
		"task is terminal; intervene/retry is unavailable",
		"ax tasks remove bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb --expected-version 3",
	} {
		if !strings.Contains(text, want) {
			t.Fatalf("expected %q in recovery preview %q", want, text)
		}
	}
}

func TestComputeTaskListViewportKeepsSelectionVisible(t *testing.T) {
	tests := []struct {
		name       string
		totalItems int
		selected   int
		height     int
		wantStart  int
		wantEnd    int
		wantVis    int
	}{
		{
			name:       "empty list still reports minimum viewport",
			totalItems: 0,
			selected:   0,
			height:     6,
			wantStart:  0,
			wantEnd:    0,
			wantVis:    3,
		},
		{
			name:       "short list shows everything",
			totalItems: 3,
			selected:   1,
			height:     10,
			wantStart:  0,
			wantEnd:    3,
			wantVis:    3,
		},
		{
			name:       "overflow scrolls selected row into view",
			totalItems: 8,
			selected:   5,
			height:     6,
			wantStart:  3,
			wantEnd:    6,
			wantVis:    3,
		},
		{
			name:       "selection is clamped at the end",
			totalItems: 8,
			selected:   99,
			height:     6,
			wantStart:  5,
			wantEnd:    8,
			wantVis:    3,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := computeTaskListViewport(tc.totalItems, tc.selected, tc.height)
			if got.Start != tc.wantStart || got.End != tc.wantEnd || got.Visible != tc.wantVis {
				t.Fatalf("unexpected viewport: got %+v want start=%d end=%d visible=%d", got, tc.wantStart, tc.wantEnd, tc.wantVis)
			}
		})
	}
}
