package mcpserver

import (
	"context"
	"encoding/json"
	"fmt"
	"regexp"
	"sort"
	"strings"
	"time"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/workspace"
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerTools(srv *server.MCPServer, client *DaemonClient, configPath string) {
	registerAgentTools(srv, client, configPath)
	registerMessageTools(srv, client, configPath)
	registerWorkspaceTools(srv, client)
	registerSharedTools(srv, client)
	registerMemoryTools(srv, client, configPath)
	registerUsageTools(srv, client, configPath)
	registerTaskTools(srv, client, configPath)

	srv.AddTool(teamStateToolDefinition(), getTeamStateHandler(client, configPath))
	srv.AddTool(teamDryRunToolDefinition(), dryRunTeamReconfigureHandler(client, configPath))
	srv.AddTool(teamApplyToolDefinition(), applyTeamReconfigureHandler(client, configPath))
}

type agentInfo struct {
	Name        string            `json:"name"`
	Dir         string            `json:"dir"`
	Description string            `json:"description,omitempty"`
	Runtime     string            `json:"runtime,omitempty"`
	LaunchMode  string            `json:"launch_mode"`
	Command     string            `json:"command,omitempty"`
	Active      bool              `json:"active"`
	State       string            `json:"state"`
	Status      types.AgentStatus `json:"status,omitempty"`
	StatusText  string            `json:"status_text,omitempty"`
	ConnectedAt *time.Time        `json:"connected_at,omitempty"`
	Instruction string            `json:"instruction_file,omitempty"`
	Preview     string            `json:"instructions_preview,omitempty"`
}

const (
	agentStateOffline = "offline"
	agentStateIdle    = "idle"
	agentStateRunning = "running"
)

type agentLifecycleAction string

const (
	agentLifecycleActionStart   agentLifecycleAction = "start"
	agentLifecycleActionStop    agentLifecycleAction = "stop"
	agentLifecycleActionRestart agentLifecycleAction = "restart"
)

type agentLifecycleTarget struct {
	Name           string
	Kind           string
	ManagedSession bool
	Workspace      *config.Workspace
	Orchestrator   *workspace.DesiredOrchestrator
	Limit          string
}

type agentLifecycleResult struct {
	Name                string `json:"name"`
	Action              string `json:"action"`
	TargetKind          string `json:"target_kind"`
	ManagedSession      bool   `json:"managed_session"`
	ExactMatch          bool   `json:"exact_match"`
	Status              string `json:"status"`
	SessionExistsBefore bool   `json:"session_exists_before"`
	SessionExistsAfter  bool   `json:"session_exists_after"`
}

type lifecycleWorkspaceManager interface {
	Create(name string, ws config.Workspace) error
	Restart(name string, ws config.Workspace) error
	Destroy(name, dir string) error
}

var (
	workspaceIsIdle              = tmux.IsIdle
	lifecycleSessionExists       = tmux.SessionExists
	newLifecycleWorkspaceManager = func(socketPath, configPath string) lifecycleWorkspaceManager {
		return workspace.NewManager(socketPath, configPath)
	}
	ensureLifecycleOrchestrator       = workspace.EnsureOrchestrator
	cleanupLifecycleOrchestratorState = workspace.CleanupOrchestratorState
)

type workspaceListResult struct {
	Workspace  string                `json:"workspace"`
	Count      int                   `json:"count"`
	Workspaces []types.WorkspaceInfo `json:"workspaces"`
}

func listAgentsHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		cfgPath, cfg, err := loadToolConfig(client, configPath)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to load ax config: %v", err)), nil
		}

		activeWorkspaces, err := client.ListWorkspaces()
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to list active workspaces: %v", err)), nil
		}

		activeByName := make(map[string]types.WorkspaceInfo, len(activeWorkspaces))
		for _, ws := range activeWorkspaces {
			activeByName[ws.Name] = ws
		}

		query := strings.TrimSpace(strings.ToLower(request.GetString("query", "")))
		activeOnly := request.GetBool("active_only", false)

		agents := make([]agentInfo, 0, len(cfg.Workspaces))
		for name, ws := range cfg.Workspaces {
			info := agentInfo{
				Name:        name,
				Dir:         ws.Dir,
				Description: ws.Description,
				Active:      false,
				State:       agentStateOffline,
				Preview:     instructionPreview(ws.Instructions),
			}

			switch {
			case ws.Agent == "none":
				info.LaunchMode = "manual"
			case strings.TrimSpace(ws.Agent) != "":
				info.LaunchMode = "custom"
				info.Command = ws.Agent
			default:
				runtime := agent.NormalizeRuntime(ws.Runtime)
				info.LaunchMode = "runtime"
				info.Runtime = runtime
				instructionFile, err := agent.InstructionFile(runtime)
				if err == nil {
					info.Instruction = instructionFile
				}
			}

			if active, ok := activeByName[name]; ok {
				info.Active = true
				info.State = deriveAgentState(name)
				info.Status = active.Status
				info.StatusText = active.StatusText
				info.ConnectedAt = active.ConnectedAt
			}

			if activeOnly && !info.Active {
				continue
			}
			if query != "" && !matchesAgentQuery(info, query) {
				continue
			}

			agents = append(agents, info)
		}

		sort.Slice(agents, func(i, j int) bool {
			return agents[i].Name < agents[j].Name
		})

		data, _ := json.MarshalIndent(map[string]any{
			"project":      cfg.Project,
			"config_path":  cfgPath,
			"agent_count":  len(agents),
			"active_count": len(activeWorkspaces),
			"agents":       agents,
		}, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func getUsageTrendsHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		workspaceName := strings.TrimSpace(request.GetString("workspace", ""))
		sinceMinutes := int(request.GetFloat("since_minutes", 180))
		bucketMinutes := int(request.GetFloat("bucket_minutes", 5))

		requests, err := buildUsageTrendRequests(client, configPath, workspaceName)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		trends, err := client.GetUsageTrends(requests, sinceMinutes, bucketMinutes)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to query usage trends: %v", err)), nil
		}

		payload := map[string]any{
			"workspace":      workspaceName,
			"since_minutes":  sinceMinutes,
			"bucket_minutes": bucketMinutes,
			"trends":         trends,
		}
		data, _ := json.MarshalIndent(payload, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func inspectAgentHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		name, _ := request.RequireString("name")
		question := strings.TrimSpace(request.GetString("question", ""))
		timeout := int(request.GetFloat("timeout", 120))

		cfgPath, cfg, err := loadToolConfig(client, configPath)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to load ax config: %v", err)), nil
		}

		ws, ok := cfg.Workspaces[name]
		if !ok {
			return mcp.NewToolResultError(fmt.Sprintf("Agent %q is not defined in %s", name, cfgPath)), nil
		}

		if question == "" {
			question = "현재 운영 상태를 간단히 요약해줘. 담당 역할, 최근 작업, 대기 중인 이슈, 핵심 지표나 리스크가 있으면 함께 알려줘. 포트폴리오 개발팀을 맡고 있다면 담당 서비스나 기능, 최근 변경사항, 남은 개발 과제, 배포 또는 운영 리스크를 짧게 포함해줘."
		}

		fullMessage := question + "\n\n[ax] 작업 완료 후 반드시 send_message(to=\"" + client.workspace + "\") 로 결과를 보내주세요."

		sendResult, err := client.SendMessage(name, fullMessage)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to send inspection request: %v", err)), nil
		}
		if sendResult.Suppressed {
			return mcp.NewToolResultText(fmt.Sprintf("Inspection request to %q was suppressed as a duplicate no-op/status update.", name)), nil
		}

		dispatchConfigPath, err := resolveToolConfigPath(client, configPath)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to resolve wake config: %v", err)), nil
		}
		if err := dispatchRunnableTarget(client.socketPath, dispatchConfigPath, name, client.workspace, false); err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Inspection request to %q was queued but wake failed: %v", name, err)), nil
		}

		reply, err := waitForWorkspaceReply(ctx, client, name, timeout)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		data, _ := json.MarshalIndent(map[string]any{
			"agent":        name,
			"description":  ws.Description,
			"question":     question,
			"status_reply": reply,
		}, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func agentLifecycleHandler(client *DaemonClient, configPath string, action agentLifecycleAction) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		name, err := request.RequireString("name")
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		target, resolvedConfigPath, err := resolveAgentLifecycleTarget(client, configPath, name)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		result, err := applyAgentLifecycleAction(client.socketPath, resolvedConfigPath, target, action)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		data, _ := json.MarshalIndent(result, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func resolveAgentLifecycleTarget(client *DaemonClient, configPath, name string) (agentLifecycleTarget, string, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return agentLifecycleTarget{}, "", fmt.Errorf("name is required")
	}

	cfgPath, cfg, err := loadToolConfig(client, configPath)
	if err != nil {
		return agentLifecycleTarget{}, "", fmt.Errorf("load ax config: %w", err)
	}

	tree, err := config.LoadTree(cfgPath)
	if err != nil {
		return agentLifecycleTarget{}, "", fmt.Errorf("load config tree: %w", err)
	}
	includeRoot := tree == nil || !tree.DisableRootOrchestrator
	desired, err := workspace.BuildDesiredState(cfg, tree, client.socketPath, cfgPath, includeRoot)
	if err != nil {
		return agentLifecycleTarget{}, "", fmt.Errorf("build desired state: %w", err)
	}

	if entry, ok := desired.Workspaces[name]; ok {
		ws := entry.Workspace
		return agentLifecycleTarget{
			Name:           name,
			Kind:           "workspace",
			ManagedSession: true,
			Workspace:      &ws,
		}, cfgPath, nil
	}

	if entry, ok := desired.Orchestrators[name]; ok {
		target := agentLifecycleTarget{
			Name:           name,
			Kind:           "orchestrator",
			ManagedSession: entry.ManagedSession,
			Orchestrator:   &entry,
		}
		if entry.Root || !entry.ManagedSession {
			target.Limit = "root orchestrator lifecycle is not supported here because it is not a daemon-managed session"
		}
		return target, cfgPath, nil
	}

	return agentLifecycleTarget{}, cfgPath, fmt.Errorf("Agent %q is not defined exactly in %s; use list_agents for exact configured names", name, cfgPath)
}

func applyAgentLifecycleAction(socketPath, configPath string, target agentLifecycleTarget, action agentLifecycleAction) (*agentLifecycleResult, error) {
	if strings.TrimSpace(target.Limit) != "" {
		return nil, fmt.Errorf("Agent %q does not support %s: %s", target.Name, action, target.Limit)
	}

	existedBefore := lifecycleSessionExists(target.Name)
	result := &agentLifecycleResult{
		Name:                target.Name,
		Action:              string(action),
		TargetKind:          target.Kind,
		ManagedSession:      target.ManagedSession,
		ExactMatch:          true,
		SessionExistsBefore: existedBefore,
	}

	switch target.Kind {
	case "workspace":
		if target.Workspace == nil {
			return nil, fmt.Errorf("workspace target %q is missing configuration", target.Name)
		}
		manager := newLifecycleWorkspaceManager(socketPath, configPath)
		switch action {
		case agentLifecycleActionStart:
			if existedBefore {
				result.Status = "already_running"
				break
			}
			if err := manager.Create(target.Name, *target.Workspace); err != nil {
				return nil, fmt.Errorf("start workspace %q: %w", target.Name, err)
			}
			result.Status = "started"
		case agentLifecycleActionStop:
			if err := manager.Destroy(target.Name, target.Workspace.Dir); err != nil {
				return nil, fmt.Errorf("stop workspace %q: %w", target.Name, err)
			}
			if existedBefore {
				result.Status = "stopped"
			} else {
				result.Status = "already_stopped"
			}
		case agentLifecycleActionRestart:
			if err := manager.Restart(target.Name, *target.Workspace); err != nil {
				return nil, fmt.Errorf("restart workspace %q: %w", target.Name, err)
			}
			result.Status = "restarted"
		default:
			return nil, fmt.Errorf("unsupported lifecycle action %q", action)
		}
	case "orchestrator":
		if target.Orchestrator == nil || target.Orchestrator.Node == nil {
			return nil, fmt.Errorf("orchestrator target %q is missing project metadata", target.Name)
		}
		switch action {
		case agentLifecycleActionStart:
			if existedBefore {
				result.Status = "already_running"
				break
			}
			if err := ensureLifecycleOrchestrator(target.Orchestrator.Node, target.Orchestrator.ParentName, socketPath, configPath, true); err != nil {
				return nil, fmt.Errorf("start orchestrator %q: %w", target.Name, err)
			}
			result.Status = "started"
		case agentLifecycleActionStop:
			if err := cleanupLifecycleOrchestratorState(target.Name, target.Orchestrator.ArtifactDir); err != nil {
				return nil, fmt.Errorf("stop orchestrator %q: %w", target.Name, err)
			}
			if existedBefore {
				result.Status = "stopped"
			} else {
				result.Status = "already_stopped"
			}
		case agentLifecycleActionRestart:
			if err := cleanupLifecycleOrchestratorState(target.Name, target.Orchestrator.ArtifactDir); err != nil {
				return nil, fmt.Errorf("restart orchestrator %q: %w", target.Name, err)
			}
			if err := ensureLifecycleOrchestrator(target.Orchestrator.Node, target.Orchestrator.ParentName, socketPath, configPath, true); err != nil {
				return nil, fmt.Errorf("restart orchestrator %q: %w", target.Name, err)
			}
			result.Status = "restarted"
		default:
			return nil, fmt.Errorf("unsupported lifecycle action %q", action)
		}
	default:
		return nil, fmt.Errorf("unsupported lifecycle target kind %q", target.Kind)
	}

	result.SessionExistsAfter = lifecycleSessionExists(target.Name)
	switch action {
	case agentLifecycleActionStart, agentLifecycleActionRestart:
		if !result.SessionExistsAfter {
			return nil, fmt.Errorf("%s %q completed without leaving a running session", action, target.Name)
		}
	case agentLifecycleActionStop:
		if result.SessionExistsAfter {
			return nil, fmt.Errorf("stop %q completed but the session is still running", target.Name)
		}
	}

	return result, nil
}

func resolveToolConfigPath(client *DaemonClient, configPath string) (string, error) {
	cfgPath, err := resolveBaseToolConfigPath(configPath)
	if err != nil {
		return "", err
	}

	if client != nil {
		if state, err := client.GetTeamState(cfgPath); err == nil && state != nil {
			if state.FeatureEnabled && strings.TrimSpace(state.EffectiveConfigPath) != "" {
				return state.EffectiveConfigPath, nil
			}
		}
	}

	return cfgPath, nil
}

func resolveBaseToolConfigPath(configPath string) (string, error) {
	cfgPath := strings.TrimSpace(configPath)
	if cfgPath != "" {
		return cfgPath, nil
	}

	var err error
	cfgPath, err = config.FindConfigFile()
	if err != nil {
		return "", err
	}
	return cfgPath, nil
}

func loadToolConfig(client *DaemonClient, configPath string) (string, *config.Config, error) {
	loadPath, err := resolveToolConfigPath(client, configPath)
	if err != nil {
		return "", nil, err
	}

	cfg, err := config.Load(loadPath)
	if err != nil {
		return "", nil, err
	}
	return loadPath, cfg, nil
}

func buildUsageTrendRequests(client *DaemonClient, configPath, workspaceName string) ([]daemon.UsageTrendWorkspace, error) {
	active, err := client.ListWorkspaces()
	if err != nil {
		return nil, fmt.Errorf("list active workspaces: %w", err)
	}
	activeByName := make(map[string]types.WorkspaceInfo, len(active))
	for _, ws := range active {
		activeByName[ws.Name] = ws
	}

	if workspaceName != "" {
		if ws, ok := activeByName[workspaceName]; ok && strings.TrimSpace(ws.Dir) != "" {
			return []daemon.UsageTrendWorkspace{{
				Workspace: workspaceName,
				Cwd:       strings.TrimSpace(ws.Dir),
			}}, nil
		}

		cfgPath, cfg, err := loadToolConfig(client, configPath)
		if err != nil {
			return nil, fmt.Errorf("load ax config for workspace %q: %w", workspaceName, err)
		}
		if ws, ok := cfg.Workspaces[workspaceName]; ok && strings.TrimSpace(ws.Dir) != "" {
			return []daemon.UsageTrendWorkspace{{
				Workspace: workspaceName,
				Cwd:       strings.TrimSpace(ws.Dir),
			}}, nil
		}
		return nil, fmt.Errorf("workspace %q not found in active registry or %s", workspaceName, cfgPath)
	}

	requests := make([]daemon.UsageTrendWorkspace, 0, len(active))
	seen := make(map[string]struct{}, len(active))
	for _, ws := range active {
		if _, ok := seen[ws.Name]; ok {
			continue
		}
		seen[ws.Name] = struct{}{}
		if strings.TrimSpace(ws.Dir) == "" {
			continue
		}
		requests = append(requests, daemon.UsageTrendWorkspace{
			Workspace: ws.Name,
			Cwd:       strings.TrimSpace(ws.Dir),
		})
	}
	sort.Slice(requests, func(i, j int) bool {
		return requests[i].Workspace < requests[j].Workspace
	})
	return requests, nil
}

func instructionPreview(instructions string) string {
	if strings.TrimSpace(instructions) == "" {
		return ""
	}
	parts := strings.Fields(strings.TrimSpace(instructions))
	if len(parts) > 24 {
		parts = parts[:24]
	}
	return strings.Join(parts, " ")
}

func matchesAgentQuery(info agentInfo, query string) bool {
	fields := []string{
		info.Name,
		info.Dir,
		info.Description,
		info.Runtime,
		info.Command,
		info.State,
		info.Preview,
		info.StatusText,
	}
	for _, field := range fields {
		if strings.Contains(strings.ToLower(field), query) {
			return true
		}
	}
	return false
}

func deriveAgentState(workspace string) string {
	if workspaceIsIdle(workspace) {
		return agentStateIdle
	}
	return agentStateRunning
}

func waitForWorkspaceReply(ctx context.Context, client *DaemonClient, from string, timeout int) (string, error) {
	deadline := time.Now().Add(time.Duration(timeout) * time.Second)
	for time.Now().Before(deadline) {
		select {
		case <-ctx.Done():
			return "", fmt.Errorf("Request cancelled")
		default:
		}

		msgs, err := client.ReadMessages(10, from)
		if err == nil && len(msgs) > 0 {
			var sb strings.Builder
			for _, msg := range msgs {
				sb.WriteString(msg.Content)
				sb.WriteString("\n")
			}
			return strings.TrimSpace(sb.String()), nil
		}

		time.Sleep(3 * time.Second)
	}

	return "", fmt.Errorf("Timeout: no reply from %q within %ds", from, timeout)
}

var taskIDPattern = regexp.MustCompile(`(?i)task id:\s*([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})`)

var dispatchRunnableTarget = workspace.DispatchRunnableWork

type startTaskResult = daemon.StartTaskResponse

type workspaceTaskView string

const (
	workspaceTaskViewAssigned workspaceTaskView = "assigned"
	workspaceTaskViewCreated  workspaceTaskView = "created"
	workspaceTaskViewBoth     workspaceTaskView = "both"
)

type workspaceTaskViewResult struct {
	Count int          `json:"count"`
	Tasks []types.Task `json:"tasks"`
}

type workspaceTaskListResult struct {
	Workspace       string                   `json:"workspace"`
	View            string                   `json:"view"`
	Status          string                   `json:"status,omitempty"`
	UniqueTaskCount int                      `json:"unique_task_count"`
	Assigned        *workspaceTaskViewResult `json:"assigned,omitempty"`
	Created         *workspaceTaskViewResult `json:"created,omitempty"`
}

func sendMessageHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		to, _ := request.RequireString("to")
		message, _ := request.RequireString("message")

		sendResult, _, err := sendWorkspaceMessage(client, configPath, to, message)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to send message: %v", err)), nil
		}
		if sendResult.Suppressed {
			return mcp.NewToolResultText(fmt.Sprintf("Message to %q suppressed as a duplicate no-op/status update.", to)), nil
		}

		return mcp.NewToolResultText(fmt.Sprintf("Message sent to %q (id: %s)", to, sendResult.MessageID)), nil
	}
}

func sendWorkspaceMessage(client *DaemonClient, configPath, target, message string) (*SendMessageResult, bool, error) {
	sendResult, err := client.SendMessage(target, message)
	if err != nil {
		return nil, false, err
	}
	if sendResult.Suppressed {
		return sendResult, false, nil
	}

	dispatchConfigPath, err := resolveToolConfigPath(client, configPath)
	if err != nil {
		return nil, false, err
	}

	freshStart, err := prepareFreshTaskStart(client, target, message)
	if err != nil {
		return nil, false, err
	}

	if err := dispatchRunnableTarget(client.socketPath, dispatchConfigPath, target, client.workspace, freshStart); err != nil {
		return nil, false, err
	}
	return sendResult, freshStart, nil
}

func prepareFreshTaskStart(client *DaemonClient, target, message string) (bool, error) {
	taskID, ok := extractTaskID(message)
	if !ok {
		return false, nil
	}

	task, err := client.GetTask(taskID)
	if err != nil {
		return false, nil
	}
	if task.Assignee != target || task.CreatedBy != client.workspace || task.StartMode != types.TaskStartFresh {
		return false, nil
	}
	return true, nil
}

func extractTaskID(message string) (string, bool) {
	matches := taskIDPattern.FindStringSubmatch(message)
	if len(matches) < 2 {
		return "", false
	}
	return matches[1], true
}

func readMessagesHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		limit := int(request.GetFloat("limit", 10))
		from := request.GetString("from", "")

		messages, err := client.ReadMessages(limit, from)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to read messages: %v", err)), nil
		}

		if len(messages) == 0 {
			return mcp.NewToolResultText("No pending messages."), nil
		}

		var sb strings.Builder
		sb.WriteString(fmt.Sprintf("%d message(s):\n\n", len(messages)))
		for _, msg := range messages {
			sb.WriteString(fmt.Sprintf("From: %s\nTime: %s\n%s\n\n---\n\n",
				msg.From, msg.CreatedAt.Format("15:04:05"), msg.Content))
		}

		return mcp.NewToolResultText(sb.String()), nil
	}
}

func broadcastMessageHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		message, _ := request.RequireString("message")

		recipients, err := client.BroadcastMessage(message)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to broadcast: %v", err)), nil
		}

		if len(recipients) == 0 {
			return mcp.NewToolResultText("No other workspaces to broadcast to."), nil
		}

		dispatchConfigPath, err := resolveToolConfigPath(client, configPath)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to resolve wake config: %v", err)), nil
		}

		for _, r := range recipients {
			if err := dispatchRunnableTarget(client.socketPath, dispatchConfigPath, r, client.workspace, false); err != nil {
				return mcp.NewToolResultError(fmt.Sprintf("Broadcast reached %q but wake failed: %v", r, err)), nil
			}
		}

		return mcp.NewToolResultText(fmt.Sprintf("Broadcast sent to %d workspace(s): %s",
			len(recipients), strings.Join(recipients, ", "))), nil
	}
}

func listWorkspacesHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		workspaces, err := client.ListWorkspaces()
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to list workspaces: %v", err)), nil
		}

		sort.Slice(workspaces, func(i, j int) bool {
			return workspaces[i].Name < workspaces[j].Name
		})

		if len(workspaces) == 0 {
			data, _ := json.MarshalIndent(workspaceListResult{
				Workspace:  client.workspace,
				Count:      0,
				Workspaces: []types.WorkspaceInfo{},
			}, "", "  ")
			return mcp.NewToolResultText(string(data)), nil
		}

		data, _ := json.MarshalIndent(workspaceListResult{
			Workspace:  client.workspace,
			Count:      len(workspaces),
			Workspaces: workspaces,
		}, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func setStatusHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		status, _ := request.RequireString("status")

		if err := client.SetStatus(status); err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to set status: %v", err)), nil
		}

		return mcp.NewToolResultText(fmt.Sprintf("Status updated to: %s", status)), nil
	}
}

func setSharedValueHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		key, _ := request.RequireString("key")
		value, _ := request.RequireString("value")

		if err := client.SetSharedValue(key, value); err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to set shared value: %v", err)), nil
		}

		return mcp.NewToolResultText(fmt.Sprintf("Shared value %q stored.", key)), nil
	}
}

func getSharedValueHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		key, _ := request.RequireString("key")

		value, found, err := client.GetSharedValue(key)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to get shared value: %v", err)), nil
		}

		if !found {
			return mcp.NewToolResultText(fmt.Sprintf("Key %q not found.", key)), nil
		}

		return mcp.NewToolResultText(fmt.Sprintf("%s = %s", key, value)), nil
	}
}

func listSharedValuesHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		values, err := client.ListSharedValues()
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to list shared values: %v", err)), nil
		}

		if len(values) == 0 {
			return mcp.NewToolResultText("No shared values."), nil
		}

		data, _ := json.MarshalIndent(values, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func requestHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		to, _ := request.RequireString("to")
		message, _ := request.RequireString("message")
		timeout := int(request.GetFloat("timeout", 120))

		// Include reply instruction in the message
		fullMessage := message + "\n\n[ax/request] 이 메시지는 동기 요청입니다. `" + client.workspace + "`가 당신의 응답을 기다리고 있습니다. 작업이 끝나면 즉시 `send_message(to=\"" + client.workspace + "\")`로 결과를 회신하세요. 하위 워크스페이스에 위임할 때는 `request`가 아닌 `send_message`를 병렬로 사용한 뒤 `read_messages`로 수집하세요."

		// Send message via daemon
		sendResult, _, err := sendWorkspaceMessage(client, configPath, to, fullMessage)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to send: %v", err)), nil
		}
		if sendResult.Suppressed {
			return mcp.NewToolResultError(fmt.Sprintf("Request to %q was suppressed as a duplicate no-op/status update", to)), nil
		}

		// Poll for reply
		deadline := time.Now().Add(time.Duration(timeout) * time.Second)
		for time.Now().Before(deadline) {
			select {
			case <-ctx.Done():
				return mcp.NewToolResultError("Request cancelled"), nil
			default:
			}

			msgs, err := client.ReadMessages(10, to)
			if err == nil && len(msgs) > 0 {
				var sb strings.Builder
				for _, msg := range msgs {
					sb.WriteString(msg.Content)
					sb.WriteString("\n")
				}
				return mcp.NewToolResultText(sb.String()), nil
			}

			time.Sleep(3 * time.Second)
		}

		return mcp.NewToolResultError(fmt.Sprintf("Timeout: no reply from %q within %ds", to, timeout)), nil
	}
}

func sendKeysHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		workspace, err := request.RequireString("workspace")
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}
		keys, err := request.RequireStringSlice("keys")
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Invalid keys argument: %v", err)), nil
		}
		if len(keys) == 0 {
			return mcp.NewToolResultError("keys must contain at least one entry"), nil
		}
		if !tmux.SessionExists(workspace) {
			return mcp.NewToolResultError(fmt.Sprintf("Workspace %q is not running", workspace)), nil
		}
		if err := tmux.SendKeys(workspace, keys); err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to send keys to %q: %v", workspace, err)), nil
		}
		return mcp.NewToolResultText(fmt.Sprintf("Sent %d key(s) to %q: %s", len(keys), workspace, strings.Join(keys, " "))), nil
	}
}

func interruptAgentHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		name, _ := request.RequireString("name")

		if !tmux.SessionExists(name) {
			return mcp.NewToolResultError(fmt.Sprintf("Workspace %q is not running", name)), nil
		}
		if err := tmux.InterruptWorkspace(name); err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to interrupt %q: %v", name, err)), nil
		}

		return mcp.NewToolResultText(fmt.Sprintf("Interrupt sent to %q", name)), nil
	}
}

func createTaskHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		title, _ := request.RequireString("title")
		assignee, _ := request.RequireString("assignee")
		description, parentTaskID, staleAfterSeconds, startMode, workflowMode, priority, err := parseTaskCreateOptions(request)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		task, err := client.CreateTask(title, description, assignee, parentTaskID, startMode, workflowMode, priority, staleAfterSeconds)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to create task: %v", err)), nil
		}

		data, _ := json.MarshalIndent(task, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func startTaskHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		title, _ := request.RequireString("title")
		assignee, _ := request.RequireString("assignee")
		message, _ := request.RequireString("message")

		description, parentTaskID, staleAfterSeconds, startMode, workflowMode, priority, err := parseTaskCreateOptions(request)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		dispatchBody, err := normalizeStartTaskMessage(message)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		started, err := client.StartTask(title, description, dispatchBody, assignee, parentTaskID, startMode, workflowMode, priority, staleAfterSeconds)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to start task: %v", err)), nil
		}

		data, _ := json.MarshalIndent(started, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func parseTaskCreateOptions(request mcp.CallToolRequest) (string, string, int, types.TaskStartMode, types.TaskWorkflowMode, types.TaskPriority, error) {
	description := request.GetString("description", "")
	parentTaskID := strings.TrimSpace(request.GetString("parent_task_id", ""))
	staleAfterSeconds := int(request.GetFloat("stale_after_seconds", 0))
	if staleAfterSeconds < 0 {
		return "", "", 0, "", "", "", fmt.Errorf("Invalid stale_after_seconds: must be >= 0")
	}

	startMode, err := parseTaskStartMode(request.GetString("start_mode", string(types.TaskStartDefault)))
	if err != nil {
		return "", "", 0, "", "", "", err
	}
	workflowMode, err := parseTaskWorkflowMode(request.GetString("workflow_mode", string(types.TaskWorkflowParallel)))
	if err != nil {
		return "", "", 0, "", "", "", err
	}
	priority, err := parseTaskPriority(request.GetString("priority", string(types.TaskPriorityNormal)))
	if err != nil {
		return "", "", 0, "", "", "", err
	}

	return description, parentTaskID, staleAfterSeconds, startMode, workflowMode, priority, nil
}

func parseTaskStartMode(value string) (types.TaskStartMode, error) {
	startMode := types.TaskStartMode(strings.TrimSpace(value))
	switch startMode {
	case "", types.TaskStartDefault:
		return types.TaskStartDefault, nil
	case types.TaskStartFresh:
		return types.TaskStartFresh, nil
	default:
		return "", fmt.Errorf("Invalid start_mode: %q (must be default or fresh)", startMode)
	}
}

func parseTaskWorkflowMode(value string) (types.TaskWorkflowMode, error) {
	workflowMode := types.TaskWorkflowMode(strings.TrimSpace(value))
	switch workflowMode {
	case "", types.TaskWorkflowParallel:
		return types.TaskWorkflowParallel, nil
	case types.TaskWorkflowSerial:
		return types.TaskWorkflowSerial, nil
	default:
		return "", fmt.Errorf("Invalid workflow_mode: %q (must be parallel or serial)", workflowMode)
	}
}

func parseTaskPriority(value string) (types.TaskPriority, error) {
	priority := types.TaskPriority(strings.TrimSpace(value))
	switch priority {
	case "", types.TaskPriorityNormal:
		return types.TaskPriorityNormal, nil
	case types.TaskPriorityLow, types.TaskPriorityHigh, types.TaskPriorityUrgent:
		return priority, nil
	default:
		return "", fmt.Errorf("Invalid priority: %q (must be low, normal, high, or urgent)", priority)
	}
}

func parseListTaskStatusFilter(value string) (*types.TaskStatus, error) {
	statusValue := strings.TrimSpace(value)
	if statusValue == "" {
		return nil, nil
	}

	status := types.TaskStatus(statusValue)
	switch status {
	case types.TaskPending, types.TaskInProgress, types.TaskCompleted, types.TaskFailed, types.TaskCancelled:
		return &status, nil
	default:
		return nil, fmt.Errorf("Invalid status filter: %q", statusValue)
	}
}

func parseWorkspaceTaskView(value string) (workspaceTaskView, error) {
	viewValue := strings.TrimSpace(value)
	view := workspaceTaskView(viewValue)
	switch view {
	case "", workspaceTaskViewBoth:
		return workspaceTaskViewBoth, nil
	case workspaceTaskViewAssigned, workspaceTaskViewCreated:
		return view, nil
	default:
		return "", fmt.Errorf("Invalid workspace task view: %q (must be assigned, created, or both)", viewValue)
	}
}

func normalizeStartTaskMessage(message string) (string, error) {
	trimmed := strings.TrimSpace(message)
	if trimmed == "" {
		return "", fmt.Errorf("message is required")
	}
	if existingTaskID, ok := extractTaskID(trimmed); ok {
		return "", fmt.Errorf("message must not include Task ID %q; start_task injects the new task ID automatically", existingTaskID)
	}
	return trimmed, nil
}

func updateTaskHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		id, _ := request.RequireString("id")
		statusStr := request.GetString("status", "")
		resultStr := request.GetString("result", "")
		logStr := request.GetString("log", "")

		var status *types.TaskStatus
		if statusStr != "" {
			s := types.TaskStatus(statusStr)
			switch s {
			case types.TaskPending, types.TaskInProgress, types.TaskCompleted, types.TaskFailed:
				status = &s
			default:
				return mcp.NewToolResultError(fmt.Sprintf("Invalid status: %q (must be pending, in_progress, completed, or failed)", statusStr)), nil
			}
		}

		var result *string
		if resultStr != "" {
			result = &resultStr
		}
		var logMsg *string
		if logStr != "" {
			logMsg = &logStr
		}

		task, err := client.UpdateTask(id, status, result, logMsg)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to update task: %v", err)), nil
		}

		data, _ := json.MarshalIndent(task, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func getTaskHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		id, _ := request.RequireString("id")

		task, err := client.GetTask(id)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to get task: %v", err)), nil
		}

		data, _ := json.MarshalIndent(task, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func listTasksHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		assignee := request.GetString("assignee", "")
		createdBy := request.GetString("created_by", "")
		status, err := parseListTaskStatusFilter(request.GetString("status", ""))
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		tasks, err := client.ListTasks(assignee, createdBy, status)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to list tasks: %v", err)), nil
		}

		if len(tasks) == 0 {
			return mcp.NewToolResultText("No tasks found."), nil
		}

		data, _ := json.MarshalIndent(map[string]any{
			"count": len(tasks),
			"tasks": tasks,
		}, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func listWorkspaceTasksHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		workspace, _ := request.RequireString("workspace")
		workspace = strings.TrimSpace(workspace)
		if workspace == "" {
			return mcp.NewToolResultError("workspace is required"), nil
		}

		view, err := parseWorkspaceTaskView(request.GetString("view", string(workspaceTaskViewBoth)))
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		status, err := parseListTaskStatusFilter(request.GetString("status", ""))
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		result := workspaceTaskListResult{
			Workspace: workspace,
			View:      string(view),
		}
		if status != nil {
			result.Status = string(*status)
		}

		uniqueTaskIDs := make(map[string]struct{})
		addUniqueTasks := func(tasks []types.Task) {
			for _, task := range tasks {
				uniqueTaskIDs[task.ID] = struct{}{}
			}
		}

		if view == workspaceTaskViewAssigned || view == workspaceTaskViewBoth {
			tasks, err := client.ListTasksAssignedToWorkspace(workspace, status)
			if err != nil {
				return mcp.NewToolResultError(fmt.Sprintf("Failed to list tasks assigned to workspace %q: %v", workspace, err)), nil
			}
			result.Assigned = &workspaceTaskViewResult{
				Count: len(tasks),
				Tasks: tasks,
			}
			addUniqueTasks(tasks)
		}

		if view == workspaceTaskViewCreated || view == workspaceTaskViewBoth {
			tasks, err := client.ListTasksCreatedByWorkspace(workspace, status)
			if err != nil {
				return mcp.NewToolResultError(fmt.Sprintf("Failed to list tasks created by workspace %q: %v", workspace, err)), nil
			}
			result.Created = &workspaceTaskViewResult{
				Count: len(tasks),
				Tasks: tasks,
			}
			addUniqueTasks(tasks)
		}

		result.UniqueTaskCount = len(uniqueTaskIDs)

		data, _ := json.MarshalIndent(result, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func cancelTaskHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		id, _ := request.RequireString("id")
		reason := request.GetString("reason", "")
		var expectedVersion *int64
		if request.GetFloat("expected_version", 0) > 0 {
			v := int64(request.GetFloat("expected_version", 0))
			expectedVersion = &v
		}

		task, err := client.CancelTask(id, reason, expectedVersion)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to cancel task: %v", err)), nil
		}
		data, _ := json.MarshalIndent(task, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func removeTaskHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		id, _ := request.RequireString("id")
		reason := request.GetString("reason", "")
		var expectedVersion *int64
		if request.GetFloat("expected_version", 0) > 0 {
			v := int64(request.GetFloat("expected_version", 0))
			expectedVersion = &v
		}

		task, err := client.RemoveTask(id, reason, expectedVersion)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to remove task: %v", err)), nil
		}
		data, _ := json.MarshalIndent(task, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func interveneTaskHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		id, _ := request.RequireString("id")
		action, _ := request.RequireString("action")
		note := request.GetString("note", "")
		var expectedVersion *int64
		if request.GetFloat("expected_version", 0) > 0 {
			v := int64(request.GetFloat("expected_version", 0))
			expectedVersion = &v
		}

		resp, err := client.InterveneTask(id, action, note, expectedVersion)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to intervene task: %v", err)), nil
		}
		data, _ := json.MarshalIndent(resp, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}
