package mcpserver

import (
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerTaskTools(srv *server.MCPServer, client *DaemonClient) {
	srv.AddTool(
		mcp.NewTool("create_task",
			mcp.WithDescription("Create a task and assign it to a workspace agent. The task tracks status (pending/in_progress/completed/failed), progress logs, and results."),
			mcp.WithString("title", mcp.Required(), mcp.Description("Short task title")),
			mcp.WithString("description", mcp.Description("Detailed task description")),
			mcp.WithString("assignee", mcp.Required(), mcp.Description("Workspace name to assign the task to")),
			mcp.WithString("start_mode", mcp.Description("Task start mode: `default` for normal session reuse, or `fresh` to recreate the worker session before first processing when this task ID is referenced in the dispatch message.")),
			mcp.WithString("priority", mcp.Description("Optional task priority: `low`, `normal`, `high`, or `urgent`. Defaults to `normal`.")),
			mcp.WithNumber("stale_after_seconds", mcp.Description("Optional staleness threshold. When >0, daemon task snapshots will mark the task stale if no progress update arrives within this many seconds while the task is still pending or in_progress.")),
		),
		createTaskHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("update_task",
			mcp.WithDescription("Update a task's status, result, or append a progress log entry."),
			mcp.WithString("id", mcp.Required(), mcp.Description("Task ID")),
			mcp.WithString("status", mcp.Description("New status: pending, in_progress, completed, or failed")),
			mcp.WithString("result", mcp.Description("Task result summary (typically set on completion)")),
			mcp.WithString("log", mcp.Description("Progress log message to append")),
		),
		updateTaskHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("get_task",
			mcp.WithDescription("Get detailed information about a specific task including its logs."),
			mcp.WithString("id", mcp.Required(), mcp.Description("Task ID")),
		),
		getTaskHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("list_tasks",
			mcp.WithDescription("List tasks with optional filters. Returns all tasks if no filters are specified."),
			mcp.WithString("assignee", mcp.Description("Filter by assigned workspace")),
			mcp.WithString("created_by", mcp.Description("Filter by creator workspace")),
			mcp.WithString("status", mcp.Description("Filter by status: pending, in_progress, completed, or failed")),
		),
		listTasksHandler(client),
	)
}
