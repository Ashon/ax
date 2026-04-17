package cmd

import (
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/usage"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	xansi "github.com/charmbracelet/x/ansi"
)

type stubWatchLifecycleClient struct {
	configPath string
	name       string
	action     types.LifecycleAction
	err        error
}

func (c *stubWatchLifecycleClient) ControlLifecycle(configPath, name string, action types.LifecycleAction) (*daemon.ControlLifecycleResponse, error) {
	c.configPath = configPath
	c.name = name
	c.action = action
	if c.err != nil {
		return nil, c.err
	}
	return &daemon.ControlLifecycleResponse{
		Target: types.LifecycleTarget{
			Name:           name,
			Kind:           types.LifecycleTargetWorkspace,
			ManagedSession: true,
		},
		Action:  action,
		Running: action != types.LifecycleActionStop,
	}, nil
}

func (c *stubWatchLifecycleClient) Close() error { return nil }

func TestResolveWatchInitialViewDefaultAndSelections(t *testing.T) {
	cases := []struct {
		name     string
		agents   bool
		tasks    bool
		messages bool
		tokens   bool
		want     streamView
		only     bool
	}{
		{name: "default", want: streamMessages},
		{name: "agents", agents: true, want: streamHidden},
		{name: "tasks", tasks: true, want: streamTasks, only: true},
		{name: "messages", messages: true, want: streamMessages, only: true},
		{name: "tokens", tokens: true, want: streamTokens, only: true},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, only, err := resolveWatchInitialView(tc.agents, tc.tasks, tc.messages, tc.tokens)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got != tc.want {
				t.Fatalf("stream = %v, want %v", got, tc.want)
			}
			if only != tc.only {
				t.Fatalf("streamOnly = %v, want %v", only, tc.only)
			}
		})
	}
}

func TestResolveWatchInitialViewRejectsConflictingFlags(t *testing.T) {
	_, _, err := resolveWatchInitialView(true, false, false, true)
	if err == nil {
		t.Fatal("expected conflicting watch flags to fail")
	}
	if !strings.Contains(err.Error(), "mutually exclusive") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestNewWatchModelUsesRequestedInitialView(t *testing.T) {
	model := newWatchModel(streamTokens, true)
	if model.stream != streamTokens {
		t.Fatalf("initial stream = %v, want %v", model.stream, streamTokens)
	}
	if !model.streamOnly {
		t.Fatal("expected stream-only mode to be preserved")
	}
}

func TestWatchViewStreamOnlyHidesAgentsPane(t *testing.T) {
	m := watchModel{
		width:      96,
		height:     18,
		stream:     streamTasks,
		streamOnly: true,
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
		},
		captures: map[string]string{
			"ax.cli": "SELECTED PANE CONTENT",
		},
		tasks: []types.Task{
			{
				ID:        "task-1",
				Title:     "Watch tasks view should be standalone",
				Assignee:  "ax.cli",
				Status:    types.TaskPending,
				UpdatedAt: time.Now(),
			},
		},
	}

	view := xansi.Strip(m.View())
	lines := strings.Split(view, "\n")
	if strings.Contains(view, "SELECTED PANE CONTENT") {
		t.Fatalf("did not expect selected session pane content in stream-only view %q", view)
	}
	if len(lines) > 0 && strings.Contains(lines[0], " agents ") {
		t.Fatalf("did not expect agents sidebar in stream-only view %q", view)
	}
	if !strings.Contains(view, " tasks active 1/1 ") {
		t.Fatalf("expected tasks pane in stream-only view %q", view)
	}
}

func TestWatchViewDefaultSplitKeepsAgentsPane(t *testing.T) {
	oldConfigPath := configPath
	configPath = filepath.Join(t.TempDir(), "missing.yaml")
	defer func() { configPath = oldConfigPath }()

	m := watchModel{
		width:      96,
		height:     18,
		stream:     streamTasks,
		streamOnly: false,
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
		},
		captures: map[string]string{
			"ax.cli": "SELECTED PANE CONTENT",
		},
		runtimes: map[string]string{
			"ax.cli": "codex",
		},
		tasks: []types.Task{
			{
				ID:        "task-1",
				Title:     "Watch split view should keep agent pane",
				Assignee:  "ax.cli",
				Status:    types.TaskPending,
				UpdatedAt: time.Now(),
			},
		},
		workspaceInfos: map[string]types.WorkspaceInfo{
			"ax.cli": {Name: "ax.cli", Status: types.StatusOnline},
		},
	}

	view := xansi.Strip(m.View())
	if !strings.Contains(view, "SELECTED PANE CONTENT") {
		t.Fatalf("expected selected session pane content in split view %q", view)
	}
	if !strings.Contains(view, " agents ") {
		t.Fatalf("expected agents sidebar in split view %q", view)
	}
}

func TestWatchStreamOnlyTabCyclesVisibleViews(t *testing.T) {
	m := watchModel{stream: streamTasks, streamOnly: true}

	next, _ := m.Update(tea.KeyMsg{Type: tea.KeyTab})
	got := next.(watchModel)
	if got.stream != streamTokens {
		t.Fatalf("expected tasks -> tokens in stream-only cycle, got %v", got.stream)
	}

	next, _ = got.Update(tea.KeyMsg{Type: tea.KeyTab})
	got = next.(watchModel)
	if got.stream != streamMessages {
		t.Fatalf("expected tokens -> messages in stream-only cycle, got %v", got.stream)
	}

	next, _ = got.Update(tea.KeyMsg{Type: tea.KeyTab})
	got = next.(watchModel)
	if got.stream != streamTasks {
		t.Fatalf("expected messages -> tasks in stream-only cycle, got %v", got.stream)
	}
}

func TestWatchEnterOpensQuickActionsSurface(t *testing.T) {
	oldLifecycleSupported := watchLifecycleSupported
	oldConfigPath := configPath
	watchLifecycleSupported = func(string) bool { return true }
	configPath = filepath.Join(t.TempDir(), "missing.yaml")
	defer func() {
		watchLifecycleSupported = oldLifecycleSupported
		configPath = oldConfigPath
	}()

	m := watchModel{
		width:    96,
		height:   24,
		selected: 0,
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
		},
		captures: map[string]string{
			"ax.cli": "Ready\n❯",
		},
		runtimes: map[string]string{
			"ax.cli": "codex",
		},
	}

	next, _ := m.Update(tea.KeyMsg{Type: tea.KeyEnter})
	got := next.(watchModel)
	if !got.quickActionsOpen {
		t.Fatal("expected quick actions to open on Enter")
	}
	if len(got.quickActions) != 6 {
		t.Fatalf("quick action count = %d, want 6", len(got.quickActions))
	}

	view := xansi.Strip(got.View())
	for _, want := range []string{"actions 1/6", "Inspect", "Open tasks", "Open messages", "enter run", "esc close"} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in quick-actions view %q", want, view)
		}
	}
	if strings.Count(view, "╭") < 2 || strings.Count(view, "╰") < 2 {
		t.Fatalf("expected nested quick-action border in view %q", view)
	}
}

func TestRenderSidebarAnchorsQuickActionsBelowSelectedAgent(t *testing.T) {
	oldConfigPath := configPath
	configPath = filepath.Join(t.TempDir(), "missing.yaml")
	defer func() { configPath = oldConfigPath }()

	m := watchModel{
		selected:            0,
		quickActionsOpen:    true,
		quickActionSelected: 0,
		quickActions: []watchQuickAction{
			{ID: watchQuickActionInspect, Label: "Inspect"},
			{ID: watchQuickActionTasks, Label: "Open tasks"},
			{ID: watchQuickActionMessages, Label: "Open messages"},
			{ID: watchQuickActionInterrupt, Label: "Interrupt"},
		},
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
			{Name: "ax-ax_runtime", Workspace: "ax.runtime"},
		},
		runtimes: map[string]string{
			"ax.cli":     "codex",
			"ax.runtime": "claude",
		},
		workspaceInfos: map[string]types.WorkspaceInfo{
			"ax.cli": {
				Name:       "ax.cli",
				Status:     types.StatusOnline,
				StatusText: "Selected status detail",
			},
			"ax.runtime": {
				Name:       "ax.runtime",
				Status:     types.StatusOnline,
				StatusText: "Other workspace detail",
			},
		},
	}

	view := xansi.Strip(m.renderSidebar(38, 10))
	if strings.Contains(view, "Selected status detail") {
		t.Fatalf("did not expect selected detail line while quick actions are open: %q", view)
	}

	lines := strings.Split(view, "\n")
	selectedLine := -1
	overlayLine := -1
	for i, line := range lines {
		if strings.Contains(line, "▸") && strings.Contains(line, "cli") {
			selectedLine = i
		}
		if strings.Contains(line, "actions 1/4") {
			overlayLine = i
		}
	}
	if selectedLine < 0 {
		t.Fatalf("expected selected agent line in sidebar view %q", view)
	}
	if overlayLine != selectedLine+1 {
		t.Fatalf("expected quick-action overlay directly below selected agent, got selected=%d overlay=%d in view %q", selectedLine, overlayLine, view)
	}
	if !strings.Contains(lines[overlayLine], "╭─") {
		t.Fatalf("expected bordered quick-action header at overlay line %q", lines[overlayLine])
	}
}

func TestRenderSidebarClipsQuickActionsOverlayInShortPane(t *testing.T) {
	oldConfigPath := configPath
	configPath = filepath.Join(t.TempDir(), "missing.yaml")
	defer func() { configPath = oldConfigPath }()

	m := watchModel{
		selected:            1,
		quickActionsOpen:    true,
		quickActionSelected: 0,
		quickActions: []watchQuickAction{
			{ID: watchQuickActionInspect, Label: "Inspect"},
			{ID: watchQuickActionTasks, Label: "Open tasks"},
			{ID: watchQuickActionMessages, Label: "Open messages"},
			{ID: watchQuickActionInterrupt, Label: "Interrupt"},
		},
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
			{Name: "ax-ax_runtime", Workspace: "ax.runtime"},
		},
		runtimes: map[string]string{
			"ax.cli":     "codex",
			"ax.runtime": "claude",
		},
		workspaceInfos: map[string]types.WorkspaceInfo{
			"ax.cli":     {Name: "ax.cli", Status: types.StatusOnline},
			"ax.runtime": {Name: "ax.runtime", Status: types.StatusOnline},
		},
	}

	view := xansi.Strip(m.renderSidebar(38, 6))
	if !strings.Contains(view, "actions 1/4") {
		t.Fatalf("expected clipped quick-action overlay header in short sidebar view %q", view)
	}
	for _, line := range strings.Split(view, "\n") {
		if w := lipgloss.Width(line); w > 38 {
			t.Fatalf("rendered sidebar line width %d exceeds pane width: %q", w, line)
		}
	}
}

func TestWatchQuickActionsHideUnsupportedLifecycleControls(t *testing.T) {
	oldLifecycleSupported := watchLifecycleSupported
	watchLifecycleSupported = func(string) bool { return false }
	defer func() { watchLifecycleSupported = oldLifecycleSupported }()

	m := watchModel{
		width:    96,
		height:   18,
		selected: 0,
		sessions: []tmux.SessionInfo{
			{Name: "ax-orchestrator", Workspace: "orchestrator"},
		},
		captures: map[string]string{
			"orchestrator": "Ready\n❯",
		},
		runtimes: map[string]string{
			"orchestrator": "codex",
		},
	}

	next, _ := m.Update(tea.KeyMsg{Type: tea.KeyEnter})
	got := next.(watchModel)
	if len(got.quickActions) != 4 {
		t.Fatalf("quick action count = %d, want 4", len(got.quickActions))
	}

	view := xansi.Strip(got.View())
	for _, unwanted := range []string{"Restart", "Stop"} {
		if strings.Contains(view, unwanted) {
			t.Fatalf("did not expect %q in unsupported quick-actions view %q", unwanted, view)
		}
	}
}

func TestWatchQuickActionsOpenTasksFocusesSelectedWorkspace(t *testing.T) {
	oldLifecycleSupported := watchLifecycleSupported
	watchLifecycleSupported = func(string) bool { return false }
	defer func() { watchLifecycleSupported = oldLifecycleSupported }()

	now := time.Now()
	m := watchModel{
		selected: 0,
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
		},
		tasks: []types.Task{
			{
				ID:        "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
				Title:     "Other workspace task",
				Assignee:  "ax.runtime",
				Status:    types.TaskInProgress,
				UpdatedAt: now,
				CreatedAt: now.Add(-2 * time.Minute),
			},
			{
				ID:        "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
				Title:     "Selected workspace task",
				Assignee:  "ax.cli",
				Status:    types.TaskInProgress,
				UpdatedAt: now.Add(-time.Minute),
				CreatedAt: now.Add(-3 * time.Minute),
			},
		},
	}

	next, _ := m.Update(tea.KeyMsg{Type: tea.KeyEnter})
	got := next.(watchModel)
	next, _ = got.Update(tea.KeyMsg{Type: tea.KeyDown})
	got = next.(watchModel)
	next, _ = got.Update(tea.KeyMsg{Type: tea.KeyEnter})
	got = next.(watchModel)

	if got.stream != streamTasks {
		t.Fatalf("stream = %v, want %v", got.stream, streamTasks)
	}
	if got.taskSelected != 1 {
		t.Fatalf("taskSelected = %d, want 1", got.taskSelected)
	}
	if got.quickActionsOpen {
		t.Fatal("expected quick actions to close after opening tasks")
	}
}

func TestWatchQuickActionsRestartRequiresConfirmation(t *testing.T) {
	oldLifecycleSupported := watchLifecycleSupported
	oldResolveConfigPath := watchResolveConfigPath
	oldNewClient := watchNewClient
	watchLifecycleSupported = func(string) bool { return true }
	watchResolveConfigPath = func() (string, error) { return "/tmp/ax.yaml", nil }
	client := &stubWatchLifecycleClient{}
	watchNewClient = func() (watchLifecycleClient, error) { return client, nil }
	defer func() {
		watchLifecycleSupported = oldLifecycleSupported
		watchResolveConfigPath = oldResolveConfigPath
		watchNewClient = oldNewClient
	}()

	m := watchModel{
		selected: 0,
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
		},
	}

	next, _ := m.Update(tea.KeyMsg{Type: tea.KeyEnter})
	got := next.(watchModel)
	for i := 0; i < 4; i++ {
		next, _ = got.Update(tea.KeyMsg{Type: tea.KeyDown})
		got = next.(watchModel)
	}

	next, _ = got.Update(tea.KeyMsg{Type: tea.KeyEnter})
	got = next.(watchModel)
	if !got.quickActionConfirm {
		t.Fatal("expected restart to require confirmation")
	}
	if client.action != "" {
		t.Fatalf("did not expect lifecycle action before confirmation, got %q", client.action)
	}

	next, _ = got.Update(tea.KeyMsg{Type: tea.KeyEnter})
	got = next.(watchModel)
	if got.quickActionsOpen {
		t.Fatal("expected quick actions to close after confirmed restart")
	}
	if client.action != types.LifecycleActionRestart {
		t.Fatalf("action = %q, want %q", client.action, types.LifecycleActionRestart)
	}
	if client.name != "ax.cli" {
		t.Fatalf("name = %q, want ax.cli", client.name)
	}
	if client.configPath != "/tmp/ax.yaml" {
		t.Fatalf("configPath = %q, want /tmp/ax.yaml", client.configPath)
	}
	if !strings.Contains(got.noticeText, "Restart requested for ax.cli") {
		t.Fatalf("expected restart notice, got %q", got.noticeText)
	}
}

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

func TestRenderSidebarShowsCompactRowAndSelectedDetail(t *testing.T) {
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
		tokenData: map[string]agentTokens{
			"ax.cli": {
				Workspace: "ax.cli",
				Up:        "1.2k",
				Down:      "345",
				Cost:      "$0.67",
			},
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
	lines := strings.Split(view, "\n")
	for _, line := range lines {
		if w := lipgloss.Width(line); w > 38 {
			t.Fatalf("rendered sidebar line width %d exceeds pane width: %q", w, line)
		}
	}
	if len(lines) == 0 {
		t.Fatalf("expected sidebar header line in view %q", view)
	}
	if strings.Index(lines[0], " agents ") >= strings.Index(lines[0], "↑↓ agent") {
		t.Fatalf("expected sidebar title before help in header %q", lines[0])
	}
	for _, want := range []string{
		"↑↓ agent",
		"codex",
		"Inspecting divergence",
		"↑1.2k ↓345",
		"$0.67",
	} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in sidebar view %q", want, view)
		}
	}
}

func TestRenderSidebarHidesUnselectedDetailLines(t *testing.T) {
	oldConfigPath := configPath
	configPath = filepath.Join(t.TempDir(), "missing.yaml")
	defer func() { configPath = oldConfigPath }()

	m := watchModel{
		selected:    0,
		spinnerTick: 0,
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
			{Name: "ax-ax_runtime", Workspace: "ax.runtime"},
		},
		runtimes: map[string]string{
			"ax.cli":     "codex",
			"ax.runtime": "claude",
		},
		tokenData: map[string]agentTokens{
			"ax.cli":     {Workspace: "ax.cli", Cost: "$0.67"},
			"ax.runtime": {Workspace: "ax.runtime", Cost: "$0.40"},
		},
		workspaceInfos: map[string]types.WorkspaceInfo{
			"ax.cli": {
				Name:       "ax.cli",
				Status:     types.StatusOnline,
				StatusText: "Selected status detail",
			},
			"ax.runtime": {
				Name:       "ax.runtime",
				Status:     types.StatusOnline,
				StatusText: "Unselected detail should stay hidden",
			},
		},
	}

	view := xansi.Strip(m.renderSidebar(38, 10))
	for _, want := range []string{"codex", "$0.67", "claude", "$0.40", "Selected status detail"} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in compact sidebar view %q", want, view)
		}
	}
	if strings.Contains(view, "Unselected detail should stay hidden") {
		t.Fatalf("did not expect unselected detail line in compact sidebar view %q", view)
	}
}

func TestRenderSidebarStateMarkerDistinguishesStatesAndAnimates(t *testing.T) {
	offline := xansi.Strip(renderSidebarStateMarker(sidebarAgentStateOffline, 0))
	idle := xansi.Strip(renderSidebarStateMarker(sidebarAgentStateIdle, 0))
	running0 := xansi.Strip(renderSidebarStateMarker(sidebarAgentStateRunning, 0))
	running1 := xansi.Strip(renderSidebarStateMarker(sidebarAgentStateRunning, 6))

	if offline != "○" {
		t.Fatalf("expected offline marker ○, got %q", offline)
	}
	if idle != "●" {
		t.Fatalf("expected idle marker ●, got %q", idle)
	}
	if running0 == offline || running0 == idle {
		t.Fatalf("expected running marker to differ from offline/idle, got %q", running0)
	}
	if running0 == running1 {
		t.Fatalf("expected running marker to animate, got same frame %q", running0)
	}
}

func TestWatchShouldRefreshDataRespectsThrottle(t *testing.T) {
	now := time.Now()

	if !watchShouldRefreshData(time.Time{}, now, false) {
		t.Fatal("expected zero last refresh to force a refresh")
	}
	if !watchShouldRefreshData(now, now, true) {
		t.Fatal("expected forced refresh to bypass throttle")
	}
	if watchShouldRefreshData(now, now.Add(100*time.Millisecond), false) {
		t.Fatal("expected refresh inside throttle window to be skipped")
	}
	if !watchShouldRefreshData(now, now.Add(watchDataRefreshInterval), false) {
		t.Fatal("expected refresh at throttle boundary to run")
	}
}

func TestWatchShouldRefreshSessionsUsesSeparateCadence(t *testing.T) {
	now := time.Now()
	if !watchShouldRefreshSessions(time.Time{}, now, false) {
		t.Fatal("expected zero last session refresh to force a refresh")
	}
	if watchShouldRefreshSessions(now, now.Add(500*time.Millisecond), false) {
		t.Fatal("expected session refresh inside interval to be skipped")
	}
	if !watchShouldRefreshSessions(now, now.Add(watchSessionRefreshInterval), false) {
		t.Fatal("expected session refresh at interval boundary to run")
	}
}

func TestPlanCaptureTargetsPrioritizesFocusedAndRotatesBackground(t *testing.T) {
	sessions := []tmux.SessionInfo{
		{Name: "ax-a", Workspace: "a"},
		{Name: "ax-b", Workspace: "b"},
		{Name: "ax-c", Workspace: "c"},
		{Name: "ax-d", Workspace: "d"},
	}

	targets, nextCursor := planCaptureTargets(sessions, map[string]bool{"c": true}, 0, 2)
	if got := []string{targets[0].Workspace, targets[1].Workspace, targets[2].Workspace}; strings.Join(got, ",") != "c,a,b" {
		t.Fatalf("unexpected first capture batch order: %v", got)
	}
	if nextCursor != 2 {
		t.Fatalf("expected next cursor 2, got %d", nextCursor)
	}

	targets, nextCursor = planCaptureTargets(sessions, map[string]bool{"c": true}, nextCursor, 2)
	if got := []string{targets[0].Workspace, targets[1].Workspace, targets[2].Workspace}; strings.Join(got, ",") != "c,d,a" {
		t.Fatalf("unexpected rotated capture batch order: %v", got)
	}
	if nextCursor != 1 {
		t.Fatalf("expected wrapped next cursor 1, got %d", nextCursor)
	}
}

func TestReadHistoryFileIfChangedReusesCachedEntries(t *testing.T) {
	path := filepath.Join(t.TempDir(), "history.jsonl")
	line := `{"from":"a","to":"b","content":"hello","timestamp":"2026-04-14T00:00:00Z"}` + "\n"
	if err := os.WriteFile(path, []byte(line), 0o644); err != nil {
		t.Fatalf("write history: %v", err)
	}

	entries, modTime := readHistoryFileIfChanged(path, time.Time{}, nil, 50)
	if len(entries) != 1 {
		t.Fatalf("expected one entry, got %d", len(entries))
	}

	cached := []daemon.HistoryEntry{{Content: "cached"}}
	reused, reusedModTime := readHistoryFileIfChanged(path, modTime, cached, 50)
	if reusedModTime != modTime {
		t.Fatalf("expected unchanged mod time, got %v vs %v", reusedModTime, modTime)
	}
	if len(reused) != 1 || reused[0].Content != "cached" {
		t.Fatalf("expected cached history to be reused, got %+v", reused)
	}
}

func TestRenderSidebarUsesDerivedStateMarkers(t *testing.T) {
	oldConfigPath := configPath
	configPath = filepath.Join(t.TempDir(), "missing.yaml")
	defer func() { configPath = oldConfigPath }()

	m := watchModel{
		spinnerTick: 0,
		captures: map[string]string{
			"ax.cli":     "Ready for input\n❯",
			"ax.runtime": "Thinking through changes",
		},
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
			{Name: "ax-ax_runtime", Workspace: "ax.runtime"},
		},
		runtimes: map[string]string{
			"ax.cli":     "codex",
			"ax.runtime": "claude",
		},
		workspaceInfos: map[string]types.WorkspaceInfo{
			"ax.cli":     {Name: "ax.cli", Status: types.StatusOnline},
			"ax.runtime": {Name: "ax.runtime", Status: types.StatusOnline},
		},
	}

	view := xansi.Strip(m.renderSidebar(38, 10))
	for _, want := range []string{"● cli", "⠁ runtime"} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in sidebar view %q", want, view)
		}
	}
}

func TestDeriveSidebarAgentStateRequiresActiveEvidenceForSpinner(t *testing.T) {
	now := time.Now()

	if got := deriveSidebarAgentState("Connected and waiting", time.Time{}, now); got != sidebarAgentStateIdle {
		t.Fatalf("expected plain online capture to stay idle, got %q", got)
	}
	if got := deriveSidebarAgentState("Thinking through changes", time.Time{}, now); got != sidebarAgentStateRunning {
		t.Fatalf("expected active status line to be running, got %q", got)
	}
	if got := deriveSidebarAgentState("Working without prompt", now.Add(-2*time.Second), now); got != sidebarAgentStateRunning {
		t.Fatalf("expected recent capture activity to keep spinner running, got %q", got)
	}
	if got := deriveSidebarAgentState("Working without prompt", now.Add(-10*time.Second), now); got != sidebarAgentStateIdle {
		t.Fatalf("expected stale non-idle capture to fall back to idle marker, got %q", got)
	}
}

func TestFormatSidebarTokenSummaryFallsBackToCostOnly(t *testing.T) {
	got := formatSidebarTokenSummary(agentTokens{
		Workspace: "ax.cli",
		Up:        "123.4k",
		Down:      "45.6k",
		Cost:      "$12.34",
	}, 8)
	if got != "$12.34" {
		t.Fatalf("expected cost-only fallback, got %q", got)
	}
}

func TestFooterTokenSummaryShowsTotalsIndependentOfTab(t *testing.T) {
	m := watchModel{
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
			{Name: "ax-ax_runtime", Workspace: "ax.runtime"},
			{Name: "ax-ax_docs", Workspace: "ax.docs"},
		},
		tokenData: map[string]agentTokens{
			"ax.cli": {
				Workspace: "ax.cli",
				Up:        "1.2k",
				Down:      "345",
				Cost:      "$0.67",
			},
			"ax.runtime": {
				Workspace: "ax.runtime",
				Up:        "800",
				Down:      "120",
				Cost:      "$1.13",
			},
		},
	}

	view := xansi.Strip(m.renderFooter(100))
	for _, line := range strings.Split(view, "\n") {
		if w := lipgloss.Width(line); w > 100 {
			t.Fatalf("rendered footer line width %d exceeds width: %q", w, line)
		}
	}
	for _, want := range []string{
		"2/3 agents",
		"↑2.0k",
		"↓465",
		"$1.80",
		"tab msgs/tasks/tokens/off",
	} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in footer view %q", want, view)
		}
	}
}

func TestRenderMainShowsLeftHeaderHelp(t *testing.T) {
	m := watchModel{}
	view := xansi.Strip(m.renderMain("ax.cli", "ready", 80, 6))
	lines := strings.Split(view, "\n")
	if len(lines) == 0 {
		t.Fatalf("expected main header line in view %q", view)
	}
	if strings.Index(lines[0], " ax.cli ") >= strings.Index(lines[0], "↑↓ agent") {
		t.Fatalf("expected main title before help in header %q", lines[0])
	}
	if !strings.Contains(view, "↑↓ agent") {
		t.Fatalf("expected left-side main help in view %q", view)
	}
	if !strings.Contains(view, " tab ") && !strings.Contains(view, "tab") {
		t.Fatalf("expected tab control hint in main view %q", view)
	}
}

func TestParseAgentTokensStripsANSIFromClaudeUsageLine(t *testing.T) {
	content := "\x1b[38;5;174m✻\x1b[39m \x1b[38;5;174mWhisking… \x1b[38;5;246m(1m\x1b[39m \x1b[38;5;246m50s\x1b[39m \x1b[38;5;246m·\x1b[39m \x1b[38;5;246m↓\x1b[39m \x1b[38;5;246m4.5k\x1b[39m \x1b[38;5;246mtokens\x1b[39m \x1b[38;5;246m·\x1b[39m \x1b[38;5;249mthinking\x1b[39m)\n"

	got := parseAgentTokens("ax.backend", content)
	if got.Down != "4.5k" {
		t.Fatalf("expected ANSI-stripped down tokens, got %+v", got)
	}

	status := parseAgentStatus(content)
	for _, want := range []string{"↓4.5k", "thinking"} {
		if !strings.Contains(status, want) {
			t.Fatalf("expected %q in parsed status %q", want, status)
		}
	}
}

func TestParseAgentTokensParsesDoneLineStandaloneTotal(t *testing.T) {
	content := "\x1b[38;5;246m  ⎿ \x1b[39m\x1b[38;5;246mDone (16 tool uses · 93.9k tokens · 59s)\x1b[39m\n"

	got := parseAgentTokens("ax.backend", content)
	if got.Total != "93.9k" {
		t.Fatalf("expected done-line total tokens, got %+v", got)
	}

	status := parseAgentStatus(content)
	if !strings.Contains(status, "Σ93.9k") {
		t.Fatalf("expected standalone total in parsed status, got %q", status)
	}
}

func TestFooterTokenSummaryShowsStandaloneTotalsWithoutFakeCost(t *testing.T) {
	m := watchModel{
		sessions: []tmux.SessionInfo{
			{Name: "ax-backend", Workspace: "ax.backend"},
		},
		tokenData: map[string]agentTokens{
			"ax.backend": {
				Workspace: "ax.backend",
				Total:     "93.9k",
			},
		},
	}

	summary := m.footerTokenSummary(80)
	for _, want := range []string{"1/1 agents", "Σ93.9k"} {
		if !strings.Contains(summary, want) {
			t.Fatalf("expected %q in summary %q", want, summary)
		}
	}
	if strings.Contains(summary, "$0.00") {
		t.Fatalf("did not expect fake zero cost in summary %q", summary)
	}
}

func TestTokenEntriesFromMapSortsCostTiesAndNoCostDeterministically(t *testing.T) {
	want := "ax.beta,ax.gamma,ax.epsilon,ax.alpha,ax.delta"

	for i := 0; i < 128; i++ {
		entries := tokenEntriesFromMap(map[string]agentTokens{
			"ax.alpha": {
				Workspace: "ax.alpha",
				Up:        "120",
			},
			"ax.beta": {
				Workspace: "ax.beta",
				Cost:      "$1.20",
				Down:      "80",
			},
			"ax.gamma": {
				Workspace: "ax.gamma",
				Cost:      "$1.20",
				Total:     "300",
			},
			"ax.delta": {
				Workspace: "ax.delta",
				Total:     "40",
			},
			"ax.epsilon": {
				Workspace: "ax.epsilon",
				Cost:      "$0.50",
				Up:        "20",
			},
		})

		gotNames := make([]string, 0, len(entries))
		for _, entry := range entries {
			gotNames = append(gotNames, entry.Workspace)
		}
		if got := strings.Join(gotNames, ","); got != want {
			t.Fatalf("iteration %d order = %q, want %q", i, got, want)
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
		"[/]",
		" f ",
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
	if strings.Index(lines[0], " tasks active 1/1 ") >= strings.Index(lines[0], "[/]") {
		t.Fatalf("expected task title before help in split header %q", lines[0])
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

func TestRenderTokensCombinesLiveUsageAndHistoryTrend(t *testing.T) {
	now := time.Date(2026, 4, 14, 16, 0, 0, 0, time.UTC)
	m := watchModel{
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
			{Name: "ax-ax_runtime", Workspace: "ax.runtime"},
		},
		tokenData: map[string]agentTokens{
			"ax.cli": {
				Workspace: "ax.cli",
				Up:        "1.2k",
				Down:      "345",
				Cost:      "$0.67",
			},
		},
		trendData: map[string]usage.WorkspaceTrend{
			"ax.cli": {
				Workspace:     "ax.cli",
				Available:     true,
				WindowStart:   now.Add(-24 * time.Hour),
				WindowEnd:     now,
				BucketMinutes: 180,
				Total:         usage.Tokens{Input: 2000, Output: 1000, CacheRead: 500},
				MCPProxy:      usage.MCPProxyMetrics{Total: 480, PromptTokens: 180, ToolUseTokens: 300},
				LatestTokens:  usage.Tokens{Input: 120, Output: 40},
				LatestMCPProxy: usage.MCPProxyMetrics{
					Total:         60,
					PromptTokens:  20,
					ToolUseTokens: 40,
				},
				Buckets: []usage.UsageBucket{
					{Totals: usage.Tokens{Input: 10}, MCPProxy: usage.MCPProxyMetrics{Total: 10}},
					{Totals: usage.Tokens{Input: 20}, MCPProxy: usage.MCPProxyMetrics{Total: 40}},
					{Totals: usage.Tokens{Input: 30}, MCPProxy: usage.MCPProxyMetrics{Total: 120}},
					{Totals: usage.Tokens{Input: 40}, MCPProxy: usage.MCPProxyMetrics{Total: 310}},
				},
			},
		},
	}

	view := xansi.Strip(m.renderTokens(100, 12))
	for _, want := range []string{"tab", "live usage", "history 24h", "ax.cli", "↑1.2k", "24H", "TREND", "MCP~", "~480", "~60"} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in tokens view %q", want, view)
		}
	}
	if !strings.Contains(view, "▂") && !strings.Contains(view, "▄") && !strings.Contains(view, "█") {
		t.Fatalf("expected sparkline glyphs in tokens view %q", view)
	}
}

func TestRenderTokensKeepsLiveUsageVisibleWhenHistoryUnavailable(t *testing.T) {
	m := watchModel{
		sessions: []tmux.SessionInfo{
			{Name: "ax-ax_cli", Workspace: "ax.cli"},
		},
		tokenData: map[string]agentTokens{
			"ax.cli": {
				Workspace: "ax.cli",
				Up:        "512",
				Down:      "128",
			},
		},
		trendData: map[string]usage.WorkspaceTrend{
			"ax.cli": {
				Workspace: "ax.cli",
				Error:     "no transcript",
			},
		},
	}

	view := xansi.Strip(m.renderTokens(100, 10))
	for _, want := range []string{"live usage", "↑512", "history 24h", "unavailable"} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in tokens view %q", want, view)
		}
	}
}

func TestRenderMessagesShowsHeaderHelp(t *testing.T) {
	now := time.Now()
	m := watchModel{
		msgHistory: []daemon.HistoryEntry{
			{Timestamp: now, From: "ax.orchestrator", To: "ax.cli", Content: "Task dispatch"},
		},
	}

	view := xansi.Strip(m.renderStream(90, 8))
	lines := strings.Split(view, "\n")
	if len(lines) == 0 {
		t.Fatalf("expected messages header line in view %q", view)
	}
	if strings.Index(lines[0], " messages ") >= strings.Index(lines[0], "tab") {
		t.Fatalf("expected messages title before help in header %q", lines[0])
	}
	for _, want := range []string{"tab", "q", "messages"} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in messages view %q", want, view)
		}
	}
}

func TestRenderMessagesUsesAvailableWidthForLongRows(t *testing.T) {
	now := time.Now()
	m := watchModel{
		msgHistory: []daemon.HistoryEntry{
			{
				Timestamp: now,
				From:      "ax.orchestrator",
				To:        "ax.workspace",
				Content:   strings.Repeat("payload ", 16),
			},
		},
	}

	view := xansi.Strip(m.renderStream(80, 6))
	lines := strings.Split(view, "\n")
	if len(lines) < 2 {
		t.Fatalf("expected message body line in view %q", view)
	}
	body := strings.TrimSuffix(strings.TrimPrefix(lines[1], "│"), "│")
	trailingGap := len(body) - len(strings.TrimRight(body, " "))
	if trailingGap > 4 {
		t.Fatalf("expected message content to use available width, trailing gap=%d in %q", trailingGap, lines[1])
	}
	if !strings.Contains(lines[1], "payload") {
		t.Fatalf("expected message payload in line %q", lines[1])
	}
}

func TestRenderTokensReusesUnusedLiveHeightForHistoryRows(t *testing.T) {
	sessions := make([]tmux.SessionInfo, 0, 8)
	trends := make(map[string]usage.WorkspaceTrend, 8)
	for i := 0; i < 7; i++ {
		workspace := "ax.workspace." + string(rune('a'+i))
		sessions = append(sessions, tmux.SessionInfo{Name: "ax-" + workspace, Workspace: workspace})
		trends[workspace] = usage.WorkspaceTrend{
			Workspace: workspace,
			Available: true,
			Total:     usage.Tokens{Input: int64((7 - i) * 1000)},
			Buckets: []usage.UsageBucket{
				{Totals: usage.Tokens{Input: int64((7 - i) * 100)}},
			},
		}
	}
	sessions = append(sessions, tmux.SessionInfo{Name: "ax-ax.workspace.unavailable", Workspace: "ax.workspace.unavailable"})
	trends["ax.workspace.unavailable"] = usage.WorkspaceTrend{
		Workspace: "ax.workspace.unavailable",
		Error:     "no transcript",
	}

	m := watchModel{
		sessions:  sessions,
		tokenData: map[string]agentTokens{},
		trendData: trends,
	}

	view := xansi.Strip(m.renderTokens(100, 14))
	for _, want := range []string{
		"ax.workspace.a",
		"ax.workspace.g",
		"1 workspace(s) unavailable",
	} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in tokens view %q", want, view)
		}
	}
}

func TestWatchTrendRequestsIncludeConfiguredOfflineWorkspaces(t *testing.T) {
	requests := watchTrendRequests(map[string]string{
		"ax.cli":       "/tmp/ax-cli",
		"ax.offline":   "/tmp/ax-offline",
		"blank":        "   ",
		"orchestrator": "/tmp/orchestrator",
	})

	if len(requests) != 3 {
		t.Fatalf("requests=%d, want 3: %+v", len(requests), requests)
	}
	got := []string{requests[0].Workspace, requests[1].Workspace, requests[2].Workspace}
	want := []string{"ax.cli", "ax.offline", "orchestrator"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("workspaces=%v, want %v", got, want)
	}
}

func TestTrendTokenLinesIncludeOfflineHistoricalWorkspaces(t *testing.T) {
	lines := trendTokenLines(100,
		[]tmux.SessionInfo{{Name: "ax-ax_cli", Workspace: "ax.cli"}},
		map[string]usage.WorkspaceTrend{
			"ax.cli": {
				Workspace:    "ax.cli",
				Available:    true,
				LatestTokens: usage.Tokens{Input: 50},
				Total:        usage.Tokens{Input: 100},
				MCPProxy:     usage.MCPProxyMetrics{Total: 10},
				Buckets: []usage.UsageBucket{
					{Totals: usage.Tokens{Input: 50}},
				},
			},
			"ax.offline": {
				Workspace:    "ax.offline",
				Available:    true,
				LatestTokens: usage.Tokens{Input: 250},
				Total:        usage.Tokens{Input: 500},
				MCPProxy:     usage.MCPProxyMetrics{Total: 40},
				Buckets: []usage.UsageBucket{
					{Totals: usage.Tokens{Input: 250}},
				},
			},
		},
		map[string]types.WorkspaceInfo{
			"ax.cli": {Name: "ax.cli", Status: types.StatusOnline},
		},
	)

	view := xansi.Strip(strings.Join(lines, "\n"))
	for _, want := range []string{"offline retained", "STATE", "ax.offline", "offline", "ax.cli", "online"} {
		if !strings.Contains(view, want) {
			t.Fatalf("expected %q in trend lines %q", want, view)
		}
	}
}

func TestLiveTokenLinesTotalMCPUsesReportingEntriesOnly(t *testing.T) {
	entries := tokenEntriesFromMap(map[string]agentTokens{
		"ax.cli": {Workspace: "ax.cli", Up: "200", Down: "100"},
	})
	summary := summarizeTokenEntries(entries, 2)
	lines := liveTokenLines(100, entries, summary, map[string]usage.WorkspaceTrend{
		"ax.cli": {
			Workspace:      "ax.cli",
			LatestMCPProxy: usage.MCPProxyMetrics{Total: 10},
		},
		"ax.offline": {
			Workspace:      "ax.offline",
			LatestMCPProxy: usage.MCPProxyMetrics{Total: 1000},
		},
	})

	view := xansi.Strip(strings.Join(lines, "\n"))
	if !strings.Contains(view, "TOTAL (1)") {
		t.Fatalf("expected total row in live lines %q", view)
	}
	if !strings.Contains(view, "~10") {
		t.Fatalf("expected live total MCP to use reporting entries only in %q", view)
	}
	if strings.Contains(view, "~1.0k") {
		t.Fatalf("did not expect offline-only MCP total in live lines %q", view)
	}
}
