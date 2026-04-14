package cmd

import (
	"fmt"
	"sort"
	"strings"

	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
)

func workspaceInfoMap(workspaces []types.WorkspaceInfo) map[string]types.WorkspaceInfo {
	byName := make(map[string]types.WorkspaceInfo, len(workspaces))
	for _, ws := range workspaces {
		byName[ws.Name] = ws
	}
	return byName
}

func workspaceAgentStatus(workspaces map[string]types.WorkspaceInfo, name string) string {
	info, ok := workspaces[name]
	if !ok {
		return "offline"
	}
	if info.Status == "" {
		return string(types.StatusOnline)
	}
	return string(info.Status)
}

func workspaceStatusPreview(workspaces map[string]types.WorkspaceInfo, name string, limit int) string {
	info, ok := workspaces[name]
	if !ok {
		return ""
	}
	status := strings.TrimSpace(info.StatusText)
	if status == "" {
		return ""
	}
	return truncateStr(status, limit)
}

func taskAttentionHint(summary taskSummary) string {
	var parts []string
	if summary.Diverged > 0 {
		parts = append(parts, fmt.Sprintf("%d diverged", summary.Diverged))
	}
	if summary.Stale > 0 {
		parts = append(parts, fmt.Sprintf("%d stale", summary.Stale))
	}
	if summary.QueuedMessages > 0 {
		parts = append(parts, fmt.Sprintf("%d queued message(s)", summary.QueuedMessages))
	}
	if len(parts) == 0 {
		return ""
	}
	return fmt.Sprintf("Attention: %s. Inspect with ax tasks --stale, ax tasks show <id>, or ax workspace list.", strings.Join(parts, ", "))
}

type workspaceListRow struct {
	Name        string
	Reconcile   string
	Tmux        string
	Agent       string
	StatusText  string
	Description string
}

type workspaceListView struct {
	ReconfigureEnabled bool
	Rows               []workspaceListRow
	HiddenInternal     []string
}

func buildWorkspaceListRows(sessions []tmux.SessionInfo, workspaces map[string]types.WorkspaceInfo, descriptions map[string]string, desired map[string]bool, reconfigureEnabled bool, includeInternal bool) workspaceListView {
	sessionByWorkspace := make(map[string]tmux.SessionInfo, len(sessions))
	names := make(map[string]struct{}, len(sessions)+len(workspaces))
	for _, session := range sessions {
		sessionByWorkspace[session.Workspace] = session
		names[session.Workspace] = struct{}{}
	}
	for name := range workspaces {
		names[name] = struct{}{}
	}
	for name := range desired {
		names[name] = struct{}{}
	}

	orderedNames := make([]string, 0, len(names))
	for name := range names {
		orderedNames = append(orderedNames, name)
	}
	sort.Strings(orderedNames)

	view := workspaceListView{
		ReconfigureEnabled: reconfigureEnabled,
		Rows:               make([]workspaceListRow, 0, len(orderedNames)),
	}
	for _, name := range orderedNames {
		_, hasAgent := workspaces[name]
		_, hasSession := sessionByWorkspace[name]
		row := workspaceListRow{
			Name:        name,
			Reconcile:   reconfigureRowState(name, desired, hasSession, hasAgent),
			Agent:       workspaceAgentStatus(workspaces, name),
			StatusText:  workspaceStatusPreview(workspaces, name, 40),
			Description: descriptions[name],
		}

		if session, ok := sessionByWorkspace[name]; ok {
			row.Tmux = "detached"
			if session.Attached {
				row.Tmux = "attached"
			}
			if _, ok := workspaces[name]; !ok {
				row.Agent = "no-agent"
			}
		} else {
			row.Tmux = "no-session"
		}

		if isInternalDaemonOnlyRow(row) {
			if !includeInternal {
				view.HiddenInternal = append(view.HiddenInternal, row.Name)
				continue
			}
			if row.Description == "" {
				row.Description = "internal daemon identity"
			} else {
				row.Description += " (internal)"
			}
		}

		view.Rows = append(view.Rows, row)
	}
	return view
}

func isInternalDaemonOnlyRow(row workspaceListRow) bool {
	return row.Tmux == "no-session" && strings.HasPrefix(row.Name, "_")
}

func formatHiddenInternalWorkspaceNote(names []string) string {
	if len(names) == 0 {
		return ""
	}
	label := "workspaces"
	pronoun := "them"
	if len(names) == 1 {
		label = "workspace"
		pronoun = "it"
	}
	preview := strings.Join(names, ", ")
	return fmt.Sprintf("Hidden %d internal daemon-only %s: %s. Use --internal to show %s.", len(names), label, preview, pronoun)
}
