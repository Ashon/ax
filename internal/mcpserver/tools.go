package mcpserver

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/ashon/ax/internal/agent"
	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerTools(srv *server.MCPServer, client *DaemonClient, configPath string) {
	// list_agents
	srv.AddTool(
		mcp.NewTool("list_agents",
			mcp.WithDescription("List configured ax agents from the active ax config, enriched with current active status when available. Supports filtering to help find a specific agent such as a portfolio development team agent."),
			mcp.WithString("query", mcp.Description("Optional case-insensitive search text matched against agent name, description, runtime, command, and instructions preview")),
			mcp.WithBoolean("active_only", mcp.Description("When true, return only currently active agents")),
		),
		listAgentsHandler(client, configPath),
	)

	// inspect_agent
	srv.AddTool(
		mcp.NewTool("inspect_agent",
			mcp.WithDescription("Ask a specific ax agent to summarize its current operating state. Useful after list_agents finds a target such as a portfolio development team agent."),
			mcp.WithString("name", mcp.Required(), mcp.Description("Configured agent/workspace name")),
			mcp.WithString("question", mcp.Description("Optional custom question. Defaults to asking for current operating status, recent work, risks, and key metrics.")),
			mcp.WithNumber("timeout", mcp.Description("Max seconds to wait for reply (default: 120)")),
		),
		inspectAgentHandler(client, configPath),
	)

	// send_message
	srv.AddTool(
		mcp.NewTool("send_message",
			mcp.WithDescription("Send a message to another workspace agent. Use this to coordinate with other agents working on the same project."),
			mcp.WithString("to", mcp.Required(), mcp.Description("Target workspace name")),
			mcp.WithString("message", mcp.Required(), mcp.Description("Message content to send")),
		),
		sendMessageHandler(client),
	)

	// read_messages
	srv.AddTool(
		mcp.NewTool("read_messages",
			mcp.WithDescription("Read pending messages from other workspace agents. Call this periodically to check for incoming coordination messages."),
			mcp.WithNumber("limit", mcp.Description("Max number of messages to read (default: 10)")),
			mcp.WithString("from", mcp.Description("Filter messages from a specific workspace")),
		),
		readMessagesHandler(client),
	)

	// broadcast_message
	srv.AddTool(
		mcp.NewTool("broadcast_message",
			mcp.WithDescription("Send a message to all other workspace agents."),
			mcp.WithString("message", mcp.Required(), mcp.Description("Message to broadcast")),
		),
		broadcastMessageHandler(client),
	)

	// list_workspaces
	srv.AddTool(
		mcp.NewTool("list_workspaces",
			mcp.WithDescription("List all active workspace agents and their current status."),
		),
		listWorkspacesHandler(client),
	)

	// set_status
	srv.AddTool(
		mcp.NewTool("set_status",
			mcp.WithDescription("Update your workspace status text, visible to other agents via list_workspaces."),
			mcp.WithString("status", mcp.Required(), mcp.Description("Status text describing current activity")),
		),
		setStatusHandler(client),
	)

	// set_shared_value
	srv.AddTool(
		mcp.NewTool("set_shared_value",
			mcp.WithDescription("Store a key-value pair visible to all workspace agents. Useful for sharing API endpoints, config, decisions."),
			mcp.WithString("key", mcp.Required(), mcp.Description("Key name")),
			mcp.WithString("value", mcp.Required(), mcp.Description("Value to store")),
		),
		setSharedValueHandler(client),
	)

	// get_shared_value
	srv.AddTool(
		mcp.NewTool("get_shared_value",
			mcp.WithDescription("Read a shared key-value pair set by any workspace agent."),
			mcp.WithString("key", mcp.Required(), mcp.Description("Key name to look up")),
		),
		getSharedValueHandler(client),
	)

	// list_shared_values
	srv.AddTool(
		mcp.NewTool("list_shared_values",
			mcp.WithDescription("List all shared key-value pairs across workspaces."),
		),
		listSharedValuesHandler(client),
	)

	// request — send message, wake agent, and wait for reply
	srv.AddTool(
		mcp.NewTool("request",
			mcp.WithDescription(
				"Send a task to another workspace agent and wait for the reply. "+
					"This wakes the target agent via tmux and polls for a response. "+
					"Use this instead of send_message when you need the result back.",
			),
			mcp.WithString("to", mcp.Required(), mcp.Description("Target workspace name")),
			mcp.WithString("message", mcp.Required(), mcp.Description("Task or question to send")),
			mcp.WithNumber("timeout", mcp.Description("Max seconds to wait for reply (default: 120)")),
		),
		requestHandler(client),
	)

	// interrupt_agent
	srv.AddTool(
		mcp.NewTool("interrupt_agent",
			mcp.WithDescription("Send Escape to a workspace tmux session to interrupt the agent's current interactive CLI action without killing the session."),
			mcp.WithString("name", mcp.Required(), mcp.Description("Target workspace name")),
		),
		interruptAgentHandler(client),
	)
}

type agentInfo struct {
	Name        string            `json:"name"`
	Dir         string            `json:"dir"`
	Description string            `json:"description,omitempty"`
	Runtime     string            `json:"runtime,omitempty"`
	LaunchMode  string            `json:"launch_mode"`
	Command     string            `json:"command,omitempty"`
	Active      bool              `json:"active"`
	Status      types.AgentStatus `json:"status,omitempty"`
	StatusText  string            `json:"status_text,omitempty"`
	ConnectedAt *time.Time        `json:"connected_at,omitempty"`
	Instruction string            `json:"instruction_file,omitempty"`
	Preview     string            `json:"instructions_preview,omitempty"`
}

type workspaceListResult struct {
	Workspace  string                `json:"workspace"`
	Count      int                   `json:"count"`
	Workspaces []types.WorkspaceInfo `json:"workspaces"`
}

func listAgentsHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		cfgPath, cfg, err := loadToolConfig(configPath)
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

func inspectAgentHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		name, _ := request.RequireString("name")
		question := strings.TrimSpace(request.GetString("question", ""))
		timeout := int(request.GetFloat("timeout", 120))

		cfgPath, cfg, err := loadToolConfig(configPath)
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

		_, err = client.SendMessage(name, fullMessage)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to send inspection request: %v", err)), nil
		}

		wakeAgent(name, client.workspace)

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

func loadToolConfig(configPath string) (string, *config.Config, error) {
	cfgPath := strings.TrimSpace(configPath)
	if cfgPath == "" {
		var err error
		cfgPath, err = config.FindConfigFile()
		if err != nil {
			return "", nil, err
		}
	}

	cfg, err := config.Load(cfgPath)
	if err != nil {
		return "", nil, err
	}
	return cfgPath, cfg, nil
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

func sendMessageHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		to, _ := request.RequireString("to")
		message, _ := request.RequireString("message")

		msgID, err := client.SendMessage(to, message)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to send message: %v", err)), nil
		}

		// Wake the target agent via tmux
		wakeAgent(to, client.workspace)

		return mcp.NewToolResultText(fmt.Sprintf("Message sent to %q (id: %s)", to, msgID)), nil
	}
}

func wakeAgent(target, sender string) {
	prompt := fmt.Sprintf(
		"read_messages MCP 도구로 수신 메시지를 확인하고 요청된 작업을 수행해 줘. 결과는 send_message(to=\"%s\")로 보내줘.",
		sender,
	)
	_ = tmux.WakeWorkspace(target, prompt)
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

func broadcastMessageHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		message, _ := request.RequireString("message")

		recipients, err := client.BroadcastMessage(message)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to broadcast: %v", err)), nil
		}

		if len(recipients) == 0 {
			return mcp.NewToolResultText("No other workspaces to broadcast to."), nil
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

func requestHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		to, _ := request.RequireString("to")
		message, _ := request.RequireString("message")
		timeout := int(request.GetFloat("timeout", 120))

		// Include reply instruction in the message
		fullMessage := message + "\n\n[ax] 작업 완료 후 반드시 send_message(to=\"" + client.workspace + "\") 로 결과를 보내주세요."

		// Send message via daemon
		_, err := client.SendMessage(to, fullMessage)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to send: %v", err)), nil
		}

		// Wake the target agent via tmux
		wakeAgent(to, client.workspace)

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
