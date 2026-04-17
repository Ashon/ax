package mcpserver

import (
	"github.com/ashon/ax/internal/types"
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerAgentTools(srv *server.MCPServer, client *DaemonClient, configPath string) {
	srv.AddTool(
		mcp.NewTool("list_agents",
			mcp.WithDescription("List configured ax agents from the active ax config, enriched with current active status when available. Supports filtering to help find a specific agent such as a portfolio development team agent."),
			mcp.WithString("query", mcp.Description("Optional case-insensitive search text matched against agent name, description, runtime, command, and instructions preview")),
			mcp.WithBoolean("active_only", mcp.Description("When true, return only currently active agents")),
		),
		listAgentsHandler(client, configPath),
	)

	srv.AddTool(
		mcp.NewTool("inspect_agent",
			mcp.WithDescription("Ask a specific ax agent to summarize its current operating state. Useful after list_agents finds a target such as a portfolio development team agent."),
			mcp.WithString("name", mcp.Required(), mcp.Description("Configured agent/workspace name")),
			mcp.WithString("question", mcp.Description("Optional custom question. Defaults to asking for current operating status, recent work, risks, and key metrics.")),
			mcp.WithNumber("timeout", mcp.Description("Max seconds to wait for reply (default: 120)")),
		),
		inspectAgentHandler(client, configPath),
	)

	srv.AddTool(
		mcp.NewTool("start_agent",
			mcp.WithDescription("Start a configured workspace agent or managed child orchestrator by exact name. Root orchestrator lifecycle is not supported by this MCP surface."),
			mcp.WithString("name", mcp.Required(), mcp.Description("Exact configured workspace or managed child orchestrator name")),
		),
		agentLifecycleHandler(client, configPath, types.LifecycleActionStart),
	)

	srv.AddTool(
		mcp.NewTool("stop_agent",
			mcp.WithDescription("Stop a configured workspace agent or managed child orchestrator by exact name. This removes the managed session and cleans generated artifacts for that target."),
			mcp.WithString("name", mcp.Required(), mcp.Description("Exact configured workspace or managed child orchestrator name")),
		),
		agentLifecycleHandler(client, configPath, types.LifecycleActionStop),
	)

	srv.AddTool(
		mcp.NewTool("restart_agent",
			mcp.WithDescription("Restart a configured workspace agent or managed child orchestrator by exact name from a fresh managed session. Root orchestrator lifecycle is not supported by this MCP surface."),
			mcp.WithString("name", mcp.Required(), mcp.Description("Exact configured workspace or managed child orchestrator name")),
		),
		agentLifecycleHandler(client, configPath, types.LifecycleActionRestart),
	)
}
