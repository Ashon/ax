package mcpserver

import (
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strings"
	"time"

	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerTools(srv *server.MCPServer, client *DaemonClient) {
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
}

func sendMessageHandler(client *DaemonClient) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		to, _ := request.RequireString("to")
		message, _ := request.RequireString("message")

		msgID, err := client.SendMessage(to, message)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to send message: %v", err)), nil
		}

		return mcp.NewToolResultText(fmt.Sprintf("Message sent to %q (id: %s)", to, msgID)), nil
	}
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

		if len(workspaces) == 0 {
			return mcp.NewToolResultText("No active workspaces."), nil
		}

		data, _ := json.MarshalIndent(workspaces, "", "  ")
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
		fullMessage := message + "\n\n[amux] 작업 완료 후 반드시 send_message(to=\"" + client.workspace + "\") 로 결과를 보내주세요."

		// Send message via daemon
		_, err := client.SendMessage(to, fullMessage)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to send: %v", err)), nil
		}

		// Wake the target agent via tmux send-keys
		tmuxSession := "amux-" + to
		prompt := fmt.Sprintf("read_messages MCP 도구로 수신 메시지를 확인하고 요청된 작업을 수행해 줘. 결과는 send_message(to=\"%s\")로 보내줘.", client.workspace)
		exec.Command("tmux", "send-keys", "-t", tmuxSession, prompt, "Enter").Run()

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
