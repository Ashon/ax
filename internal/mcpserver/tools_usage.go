package mcpserver

import (
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerUsageTools(srv *server.MCPServer, client *DaemonClient, configPath string) {
	srv.AddTool(
		mcp.NewTool("get_usage_trends",
			mcp.WithDescription("Return recent Claude token trend data for active ax workspaces, including per-agent breakout when transcript attribution is available."),
			mcp.WithString("workspace", mcp.Description("Optional workspace name filter. When omitted, queries all active workspaces.")),
			mcp.WithNumber("since_minutes", mcp.Description("History window in minutes. Defaults to 180.")),
			mcp.WithNumber("bucket_minutes", mcp.Description("Bucket size in minutes. Defaults to 5.")),
		),
		getUsageTrendsHandler(client, configPath),
	)
}
