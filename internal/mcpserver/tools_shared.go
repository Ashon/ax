package mcpserver

import (
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerSharedTools(srv *server.MCPServer, client *DaemonClient) {
	srv.AddTool(
		mcp.NewTool("set_shared_value",
			mcp.WithDescription("Store a key-value pair visible to all workspace agents. Useful for sharing API endpoints, config, decisions."),
			mcp.WithString("key", mcp.Required(), mcp.Description("Key name")),
			mcp.WithString("value", mcp.Required(), mcp.Description("Value to store")),
		),
		setSharedValueHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("get_shared_value",
			mcp.WithDescription("Read a shared key-value pair set by any workspace agent."),
			mcp.WithString("key", mcp.Required(), mcp.Description("Key name to look up")),
		),
		getSharedValueHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("list_shared_values",
			mcp.WithDescription("List all shared key-value pairs across workspaces."),
		),
		listSharedValuesHandler(client),
	)
}
