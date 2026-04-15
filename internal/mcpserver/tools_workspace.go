package mcpserver

import (
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerWorkspaceTools(srv *server.MCPServer, client *DaemonClient) {
	srv.AddTool(
		mcp.NewTool("list_workspaces",
			mcp.WithDescription("List all active workspace agents and their current status."),
		),
		listWorkspacesHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("set_status",
			mcp.WithDescription("Update your workspace status text, visible to other agents via list_workspaces."),
			mcp.WithString("status", mcp.Required(), mcp.Description("Status text describing current activity")),
		),
		setStatusHandler(client),
	)
}
