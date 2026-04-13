package cmd

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/charmbracelet/lipgloss"
	xansi "github.com/charmbracelet/x/ansi"
)

func TestSanitizeDisplayLineRemovesANSIAndControls(t *testing.T) {
	in := "A\aB\x1b]8;;https://example.com\x1b\\LINK\x1b]8;;\x1b\\ \x1b[31mred\x1b[0m 😀 e\u0301 ─"
	got := sanitizeDisplayLine(in)

	if strings.ContainsRune(got, '\a') {
		t.Fatalf("expected BEL to be removed: %q", got)
	}
	if strings.Contains(got, "\x1b") {
		t.Fatalf("expected ANSI/OSC escapes to be removed: %q", got)
	}
	if !strings.Contains(got, "LINK red 😀 e\u0301 ─") {
		t.Fatalf("expected visible content to remain, got %q", got)
	}
}

func TestRenderMainKeepsWidthsBoundedForUnicodeHeavyLines(t *testing.T) {
	m := watchModel{}
	content := strings.Join([]string{
		"plain ascii line",
		"A\aB\x1b]8;;https://example.com\x1b\\LINK\x1b]8;;\x1b\\ emoji 😀 ZWJ 👨‍👩‍👧‍👦 combining e\u0301 box ─",
	}, "\n")

	view := m.renderMain("ws", content, 32, 6)
	for _, line := range strings.Split(view, "\n") {
		if w := lipgloss.Width(line); w > 32 {
			t.Fatalf("rendered line width %d exceeds pane width: %q", w, line)
		}
	}
	if strings.Contains(view, "\x1b]8;") || strings.ContainsRune(view, '\a') {
		t.Fatalf("rendered view still contains unsafe control sequences: %q", view)
	}
	if !strings.Contains(view, "plain ascii line") {
		t.Fatalf("expected ASCII content to remain visible: %q", view)
	}
}

func runeIndex(s string, target rune, occurrence int) int {
	count := 0
	for i, r := range []rune(s) {
		if r != target {
			continue
		}
		count++
		if count == occurrence {
			return i
		}
	}
	return -1
}

func TestRenderSidebarShowsStatusTextAndAttention(t *testing.T) {
	oldConfigPath := configPath
	configPath = filepath.Join(t.TempDir(), "missing.yaml")
	defer func() { configPath = oldConfigPath }()

	now := time.Now()
	m := watchModel{
		selected: 0,
		captures: map[string]string{
			"ax.cli": "thinking",
		},
		activity: map[string]time.Time{
			"ax.cli": now,
		},
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
		},
		runtimes: map[string]string{
			"ax.cli": "codex",
		},
		tasks: []types.Task{
			{
				ID:        "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
				Title:     "Audit watch sidebar",
				Assignee:  "ax.cli",
				Status:    types.TaskPending,
				UpdatedAt: now.Add(-5 * time.Minute),
				StaleInfo: &types.TaskStaleInfo{
					IsStale:         true,
					StateDivergence: true,
				},
			},
		},
		workspaceInfos: map[string]types.WorkspaceInfo{
			"ax.cli": {
				Name:       "ax.cli",
				Status:     types.StatusOnline,
				StatusText: "Inspecting divergence visibility for operators",
			},
		},
	}

	view := xansi.Strip(m.renderSidebar(38, 10))
	for _, line := range strings.Split(view, "\n") {
		if w := lipgloss.Width(line); w > 38 {
			t.Fatalf("rendered sidebar line width %d exceeds pane width: %q", w, line)
		}
	}
	for _, want := range []string{
		"D1 S1",
		"Inspecting divergence",
	} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in sidebar view %q", want, view)
		}
	}
}

func TestRenderTasksShowsAttentionBadgesInList(t *testing.T) {
	now := time.Now()
	m := watchModel{
		taskFilter:   taskFilterActive,
		taskSelected: 0,
		msgHistory:   nil,
		tasks: []types.Task{
			{
				ID:        "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
				Title:     "Investigate lifecycle divergence",
				Assignee:  "ax.cli",
				CreatedBy: "ax.orchestrator",
				Status:    types.TaskPending,
				Priority:  types.TaskPriorityHigh,
				UpdatedAt: now.Add(-3 * time.Minute),
				StaleInfo: &types.TaskStaleInfo{
					IsStale:         true,
					StateDivergence: true,
					PendingMessages: 2,
				},
			},
		},
	}

	view := xansi.Strip(m.renderTasks(72, 12))
	for _, line := range strings.Split(view, "\n") {
		if w := lipgloss.Width(line); w > 72 {
			t.Fatalf("rendered task line width %d exceeds pane width: %q", w, line)
		}
	}
	for _, want := range []string{
		"DIVERGED",
		"STALE",
		"Q2",
	} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in task list view %q", want, view)
		}
	}
}

func TestRenderTasksConnectsSplitPaneDividerToBorders(t *testing.T) {
	now := time.Now()
	m := watchModel{
		taskFilter:   taskFilterActive,
		taskSelected: 0,
		msgHistory: []daemon.HistoryEntry{
			{Timestamp: now, From: "ax.orchestrator", To: "ax.cli", Content: "Task dispatch"},
		},
		tasks: []types.Task{
			{
				ID:        "cccccccc-cccc-cccc-cccc-cccccccccccc",
				Title:     "Fix divider geometry",
				Assignee:  "ax.cli",
				CreatedBy: "ax.orchestrator",
				Status:    types.TaskInProgress,
				Priority:  types.TaskPriorityHigh,
				UpdatedAt: now.Add(-2 * time.Minute),
				StaleInfo: &types.TaskStaleInfo{},
			},
		},
	}

	view := xansi.Strip(m.renderTasks(90, 12))
	lines := strings.Split(view, "\n")
	if len(lines) < 3 {
		t.Fatalf("expected multi-line split view, got %q", view)
	}
	topJunction := runeIndex(lines[0], '┬', 1)
	bottomJunction := runeIndex(lines[len(lines)-1], '┴', 1)
	bodyJunction := runeIndex(lines[1], '│', 2)
	if topJunction < 0 || bottomJunction < 0 || bodyJunction < 0 {
		t.Fatalf("expected connected split-pane junctions in view %q", view)
	}
	if topJunction != bodyJunction || bottomJunction != bodyJunction {
		t.Fatalf("expected top/body/bottom divider columns to match, got top=%d body=%d bottom=%d in view %q", topJunction, bodyJunction, bottomJunction, view)
	}
}

func TestRenderTasksScrollsSelectedTaskIntoView(t *testing.T) {
	now := time.Now()
	var tasks []types.Task
	for i := 0; i < 8; i++ {
		tasks = append(tasks, types.Task{
			ID:        strings.Repeat(string(rune('a'+i)), 8) + "-0000-0000-0000-000000000000",
			Title:     "Task viewport " + string(rune('A'+i)),
			Assignee:  "ax.cli",
			CreatedBy: "ax.orchestrator",
			Status:    types.TaskPending,
			Priority:  types.TaskPriorityNormal,
			UpdatedAt: now.Add(-time.Duration(i) * time.Minute),
		})
	}
	m := watchModel{
		taskFilter:   taskFilterAll,
		taskSelected: 5,
		tasks:        tasks,
	}

	view := xansi.Strip(m.renderTasks(90, 8))
	if strings.Contains(view, "Task viewport A") || strings.Contains(view, "Task viewport B") {
		t.Fatalf("expected early tasks to scroll out of view, got %q", view)
	}
	if !strings.Contains(view, "Task viewport F") {
		t.Fatalf("expected selected task to remain visible, got %q", view)
	}
}

func TestShellRenderTasksUsesSharedViewportWindowing(t *testing.T) {
	now := time.Now()
	var tasks []types.Task
	for i := 0; i < 7; i++ {
		tasks = append(tasks, types.Task{
			ID:        strings.Repeat(string(rune('k'+i)), 8) + "-0000-0000-0000-000000000000",
			Title:     "Shell viewport " + string(rune('A'+i)),
			Assignee:  "ax.cli",
			CreatedBy: "ax.orchestrator",
			Status:    types.TaskPending,
			Priority:  types.TaskPriorityNormal,
			UpdatedAt: now.Add(-time.Duration(i) * time.Minute),
		})
	}
	m := shellModel{
		taskFilter:   taskFilterAll,
		taskSelected: 6,
		tasks:        tasks,
	}

	view := xansi.Strip(m.renderTasks(90, 8))
	if strings.Contains(view, "Shell viewport A") || strings.Contains(view, "Shell viewport B") {
		t.Fatalf("expected shell task viewport to hide overflowed rows, got %q", view)
	}
	if !strings.Contains(view, "Shell viewport G") {
		t.Fatalf("expected selected shell task to remain visible, got %q", view)
	}
}
