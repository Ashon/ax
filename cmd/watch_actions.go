package cmd

import (
	"fmt"
	"strings"
	"time"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/workspace"
	"github.com/charmbracelet/lipgloss"
)

type watchQuickActionID string

const (
	watchQuickActionStream    watchQuickActionID = "stream"
	watchQuickActionTasks     watchQuickActionID = "tasks"
	watchQuickActionMessages  watchQuickActionID = "messages"
	watchQuickActionInterrupt watchQuickActionID = "interrupt"
	watchQuickActionRestart   watchQuickActionID = "restart"
	watchQuickActionStop      watchQuickActionID = "stop"
)

const (
	watchQuickActionViewportSize = 3
	watchNoticeDuration          = 3 * time.Second
)

type watchQuickAction struct {
	ID    watchQuickActionID
	Label string
}

type watchLifecycleClient interface {
	ControlLifecycle(configPath, name string, action types.LifecycleAction) (*daemon.ControlLifecycleResponse, error)
	Close() error
}

var (
	watchInterruptWorkspace = tmuxInterruptWorkspace
	watchResolveConfigPath  = resolveConfigPath
	watchNewClient          = func() (watchLifecycleClient, error) { return newCLIClient() }
	watchLifecycleSupported = defaultWatchLifecycleSupported
	watchDaemonRunning      = func() bool {
		return isDaemonRunning(daemonutil.ExpandSocketPath(socketPath))
	}
)

func tmuxInterruptWorkspace(workspace string) error {
	return tmux.InterruptWorkspace(workspace)
}

func defaultWatchLifecycleSupported(name string) bool {
	name = strings.TrimSpace(name)
	if name == "" || !watchDaemonRunning() {
		return false
	}

	cfgPath, err := watchResolveConfigPath()
	if err != nil {
		return false
	}

	cfg, err := config.Load(cfgPath)
	if err != nil {
		return false
	}
	tree, err := config.LoadTree(cfgPath)
	if err != nil {
		return false
	}
	includeRoot := tree == nil || !tree.DisableRootOrchestrator
	desired, err := workspace.BuildDesiredState(cfg, tree, socketPath, cfgPath, includeRoot)
	if err != nil {
		return false
	}
	if _, ok := desired.Workspaces[name]; ok {
		return true
	}
	entry, ok := desired.Orchestrators[name]
	return ok && entry.ManagedSession && !entry.Root
}

func (m watchModel) selectedWorkspaceName() string {
	if m.selected < 0 || m.selected >= len(m.sessions) {
		return ""
	}
	return m.sessions[m.selected].Workspace
}

func (m *watchModel) openQuickActions() {
	workspaceName := m.selectedWorkspaceName()
	if workspaceName == "" {
		return
	}
	m.quickActions = buildWatchQuickActions(workspaceName)
	if len(m.quickActions) == 0 {
		return
	}
	m.quickActionsOpen = true
	m.quickActionSelected = 0
	m.quickActionConfirm = false
}

func buildWatchQuickActions(workspaceName string) []watchQuickAction {
	actions := []watchQuickAction{
		{ID: watchQuickActionStream, Label: "Stream tmux"},
		{ID: watchQuickActionTasks, Label: "Open tasks"},
		{ID: watchQuickActionMessages, Label: "Open messages"},
		{ID: watchQuickActionInterrupt, Label: "Interrupt"},
	}
	if watchLifecycleSupported(workspaceName) {
		actions = append(actions,
			watchQuickAction{ID: watchQuickActionRestart, Label: "Restart"},
			watchQuickAction{ID: watchQuickActionStop, Label: "Stop"},
		)
	}
	return actions
}

func (m *watchModel) closeQuickActions() {
	m.quickActionsOpen = false
	m.quickActions = nil
	m.quickActionSelected = 0
	m.quickActionConfirm = false
}

func (m *watchModel) moveQuickActionSelection(delta int) {
	if len(m.quickActions) == 0 {
		return
	}
	m.quickActionSelected = clampIndex(m.quickActionSelected+delta, len(m.quickActions))
	m.quickActionConfirm = false
}

func (m watchModel) selectedQuickAction() (watchQuickAction, bool) {
	if len(m.quickActions) == 0 {
		return watchQuickAction{}, false
	}
	idx := clampIndex(m.quickActionSelected, len(m.quickActions))
	return m.quickActions[idx], true
}

func (m *watchModel) runSelectedQuickAction() {
	action, ok := m.selectedQuickAction()
	if !ok {
		m.closeQuickActions()
		return
	}
	if action.requiresConfirmation() && !m.quickActionConfirm {
		m.quickActionConfirm = true
		return
	}

	workspaceName := m.selectedWorkspaceName()
	if workspaceName == "" {
		m.closeQuickActions()
		return
	}

	switch action.ID {
	case watchQuickActionStream:
		m.viewMode = viewModeStream
		m.stream = streamHidden
		m.closeQuickActions()
	case watchQuickActionTasks:
		m.stream = streamTasks
		m.focusTasksForWorkspace(workspaceName)
		m.closeQuickActions()
	case watchQuickActionMessages:
		m.stream = streamMessages
		m.closeQuickActions()
	case watchQuickActionInterrupt:
		if err := watchInterruptWorkspace(workspaceName); err != nil {
			m.setNotice(err.Error(), true)
			return
		}
		m.setNotice(fmt.Sprintf("Interrupted %s", workspaceName), false)
		m.refreshAfterAgentAction()
		m.closeQuickActions()
	case watchQuickActionRestart:
		if err := m.runLifecycleAction(workspaceName, types.LifecycleActionRestart); err != nil {
			m.setNotice(err.Error(), true)
			return
		}
		m.setNotice(fmt.Sprintf("Restart requested for %s", workspaceName), false)
		m.refreshAfterAgentAction()
		m.closeQuickActions()
	case watchQuickActionStop:
		if err := m.runLifecycleAction(workspaceName, types.LifecycleActionStop); err != nil {
			m.setNotice(err.Error(), true)
			return
		}
		m.setNotice(fmt.Sprintf("Stop requested for %s", workspaceName), false)
		m.refreshAfterAgentAction()
		m.closeQuickActions()
	}
}

func (m *watchModel) runLifecycleAction(workspaceName string, action types.LifecycleAction) error {
	cfgPath, err := watchResolveConfigPath()
	if err != nil {
		return err
	}
	client, err := watchNewClient()
	if err != nil {
		return err
	}
	defer client.Close()

	_, err = client.ControlLifecycle(cfgPath, workspaceName, action)
	return err
}

func (m *watchModel) refreshAfterAgentAction() {
	m.forceDataRefresh = true
	m.sessionsRefreshedAt = time.Time{}
	m.workspaceInfoUpdatedAt = time.Time{}
}

func (m *watchModel) focusTasksForWorkspace(workspaceName string) {
	filtered := filterTasks(m.tasks, m.taskFilter)
	for i, task := range filtered {
		if task.Assignee == workspaceName {
			m.taskSelected = i
			return
		}
	}

	for _, task := range m.tasks {
		if task.Assignee != workspaceName {
			continue
		}
		m.taskFilter = taskFilterAll
		filtered = filterTasks(m.tasks, m.taskFilter)
		for i, candidate := range filtered {
			if candidate.ID == task.ID {
				m.taskSelected = i
				return
			}
		}
	}
	m.taskSelected = clampIndex(m.taskSelected, len(filtered))
}

func (m *watchModel) setNotice(text string, isError bool) {
	m.noticeText = strings.TrimSpace(text)
	m.noticeErr = isError
	if m.noticeText == "" {
		m.noticeUntil = time.Time{}
		return
	}
	m.noticeUntil = time.Now().Add(watchNoticeDuration)
}

func (m *watchModel) clearExpiredNotice(now time.Time) {
	if m.noticeText == "" || m.noticeUntil.IsZero() {
		return
	}
	if now.After(m.noticeUntil) {
		m.noticeText = ""
		m.noticeErr = false
		m.noticeUntil = time.Time{}
	}
}

func (m watchModel) quickActionHelpText() string {
	if m.quickActionsOpen {
		return " ↑↓ action · enter run · esc close · q quit"
	}
	return " ↑↓ agent · enter actions · [/ ] task · f filter · x interrupt · tab msgs/tasks/tokens/off · q quit"
}

func (m watchModel) quickActionNoticeStyle() lipgloss.Style {
	if m.noticeErr {
		return taskFailClr
	}
	return statStyle
}

func (m watchModel) quickActionViewport() (int, int) {
	if len(m.quickActions) <= watchQuickActionViewportSize {
		return 0, len(m.quickActions)
	}
	selected := clampIndex(m.quickActionSelected, len(m.quickActions))
	start := selected - 1
	if start < 0 {
		start = 0
	}
	maxStart := len(m.quickActions) - watchQuickActionViewportSize
	if start > maxStart {
		start = maxStart
	}
	return start, start + watchQuickActionViewportSize
}

func (action watchQuickAction) requiresConfirmation() bool {
	return action.ID == watchQuickActionRestart || action.ID == watchQuickActionStop
}

func (action watchQuickAction) confirmationPrompt() string {
	switch action.ID {
	case watchQuickActionRestart:
		return "confirm restart?"
	case watchQuickActionStop:
		return "confirm stop?"
	default:
		return ""
	}
}
