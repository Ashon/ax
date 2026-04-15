package mcpserver

import (
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerMessageTools(srv *server.MCPServer, client *DaemonClient, configPath string) {
	srv.AddTool(
		mcp.NewTool("send_message",
			mcp.WithDescription("Send a message to another workspace agent. Use this to coordinate with other agents working on the same project."),
			mcp.WithString("to", mcp.Required(), mcp.Description("Target workspace name")),
			mcp.WithString("message", mcp.Required(), mcp.Description("Message content to send")),
		),
		sendMessageHandler(client, configPath),
	)

	srv.AddTool(
		mcp.NewTool("read_messages",
			mcp.WithDescription("Read pending messages from other workspace agents. Call this periodically to check for incoming coordination messages."),
			mcp.WithNumber("limit", mcp.Description("Max number of messages to read (default: 10)")),
			mcp.WithString("from", mcp.Description("Filter messages from a specific workspace")),
		),
		readMessagesHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("broadcast_message",
			mcp.WithDescription("Send a message to all other workspace agents."),
			mcp.WithString("message", mcp.Required(), mcp.Description("Message to broadcast")),
		),
		broadcastMessageHandler(client),
	)

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

	srv.AddTool(
		mcp.NewTool("interrupt_agent",
			mcp.WithDescription("Send Escape to a workspace tmux session to interrupt the agent's current interactive CLI action without killing the session."),
			mcp.WithString("name", mcp.Required(), mcp.Description("Target workspace name")),
		),
		interruptAgentHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("send_keys",
			mcp.WithDescription(
				"Send a sequence of keystrokes to a workspace's tmux session. "+
					"Use this to resolve blocking interactive dialogs in an agent CLI "+
					"(e.g. Claude Code's \"Resuming from summary\" 1/2/3 prompt). "+
					"Each element is either a named special key (Enter, Escape, Tab, Space, "+
					"BSpace, Up/Down/Left/Right, Home/End, PageUp/PageDown, Ctrl-C/Ctrl-D/Ctrl-U/...) "+
					"or literal text that will be typed verbatim. "+
					"Example: keys=[\"2\",\"Enter\"] selects the second option and submits it.",
			),
			mcp.WithString("workspace", mcp.Required(), mcp.Description("Target workspace name")),
			mcp.WithArray("keys", mcp.Required(),
				mcp.Description("Ordered key sequence. Named keys (Enter, Escape, C-c, ...) are resolved as tmux key names; anything else is typed literally."),
				mcp.WithStringItems(),
			),
		),
		sendKeysHandler(client),
	)
}
