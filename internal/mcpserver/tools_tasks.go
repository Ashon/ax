package mcpserver

import (
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerTaskTools(srv *server.MCPServer, client *DaemonClient, configPath string) {
	srv.AddTool(
		mcp.NewTool("create_task",
			mcp.WithDescription("Create a task record and assign it to a workspace agent without dispatching it. Use start_task when work should begin immediately."),
			mcp.WithString("title", mcp.Required(), mcp.Description("Short task title")),
			mcp.WithString("description", mcp.Description("Detailed task description")),
			mcp.WithString("assignee", mcp.Required(), mcp.Description("Workspace name to assign the task to")),
			mcp.WithString("parent_task_id", mcp.Description("Optional parent task ID for explicit umbrella/child rollup and reconciliation.")),
			mcp.WithString("start_mode", mcp.Description("Task start mode metadata: `default` for normal session reuse, or `fresh` to recreate the worker session before the first task-aware dispatch for this task.")),
			mcp.WithString("workflow_mode", mcp.Description("Task workflow mode: `parallel` (default) or `serial`. Serial parents release child dispatches in append order.")),
			mcp.WithString("priority", mcp.Description("Optional task priority: `low`, `normal`, `high`, or `urgent`. Defaults to `normal`.")),
			mcp.WithNumber("stale_after_seconds", mcp.Description("Optional staleness threshold. When >0, daemon task snapshots will mark the task stale if no progress update arrives within this many seconds while the task is still pending or in_progress.")),
		),
		createTaskHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("start_task",
			mcp.WithDescription("Create a task and let the daemon persist or release the initial task-aware dispatch. Prefer this over create_task + send_message when work should begin immediately; serial workflow children may return `dispatch.status=\"waiting_turn\"` until prior siblings become terminal."),
			mcp.WithString("title", mcp.Required(), mcp.Description("Short task title")),
			mcp.WithString("message", mcp.Required(), mcp.Description("Initial dispatch message sent to the assignee. `Task ID:` is added automatically.")),
			mcp.WithString("description", mcp.Description("Detailed task description")),
			mcp.WithString("assignee", mcp.Required(), mcp.Description("Workspace name to assign the task to")),
			mcp.WithString("parent_task_id", mcp.Description("Optional parent task ID for explicit umbrella/child rollup and reconciliation.")),
			mcp.WithString("start_mode", mcp.Description("Task start mode: `default` for normal session reuse, or `fresh` to recreate the worker session before this initial dispatch is processed.")),
			mcp.WithString("workflow_mode", mcp.Description("Task workflow mode: `parallel` (default) or `serial`. Use `serial` for parents that should release child work in order.")),
			mcp.WithString("priority", mcp.Description("Optional task priority: `low`, `normal`, `high`, or `urgent`. Defaults to `normal`.")),
			mcp.WithNumber("stale_after_seconds", mcp.Description("Optional staleness threshold. When >0, daemon task snapshots will mark the task stale if no progress update arrives within this many seconds while the task is still pending or in_progress.")),
		),
		startTaskHandler(client),
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
			mcp.WithString("status", mcp.Description("Filter by status: pending, in_progress, completed, failed, or cancelled")),
		),
		listTasksHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("cancel_task",
			mcp.WithDescription("Cancel a task via a dedicated control path. Creators, assignees, and the CLI operator may cancel active tasks."),
			mcp.WithString("id", mcp.Required(), mcp.Description("Task ID")),
			mcp.WithString("reason", mcp.Description("Optional cancellation reason recorded on the task")),
			mcp.WithNumber("expected_version", mcp.Description("Optional optimistic concurrency guard. Fails if the task version does not match.")),
		),
		cancelTaskHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("remove_task",
			mcp.WithDescription("Soft-delete/archive a terminal task so it no longer appears in list results by default."),
			mcp.WithString("id", mcp.Required(), mcp.Description("Task ID")),
			mcp.WithString("reason", mcp.Description("Optional archive reason")),
			mcp.WithNumber("expected_version", mcp.Description("Optional optimistic concurrency guard. Fails if the task version does not match.")),
		),
		removeTaskHandler(client),
	)

	srv.AddTool(
		mcp.NewTool("intervene_task",
			mcp.WithDescription("Apply a bounded, task-targeted recovery action for a stuck pending/in_progress task."),
			mcp.WithString("id", mcp.Required(), mcp.Description("Task ID")),
			mcp.WithString("action", mcp.Required(), mcp.Description("Bounded action: `wake`, `interrupt`, or `retry`.")),
			mcp.WithString("note", mcp.Description("Optional note included in retry follow-up messages.")),
			mcp.WithNumber("expected_version", mcp.Description("Optional optimistic concurrency guard. Fails if the task version does not match.")),
		),
		interveneTaskHandler(client),
	)
}
