package mcpserver

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/config"
	axmemory "github.com/ashon/ax/internal/memory"
	"github.com/ashon/ax/internal/workspace"
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func registerMemoryTools(srv *server.MCPServer, client *DaemonClient, configPath string) {
	srv.AddTool(
		mcp.NewTool("remember_memory",
			mcp.WithDescription("Persist a durable project/workspace memory in the ax daemon so it survives runtime restarts and tool changes. Use for lasting decisions, facts, constraints, handoffs, and preferences."),
			mcp.WithString("content", mcp.Required(), mcp.Description("Durable memory content to persist.")),
			mcp.WithString("scope", mcp.Description("Scope selector. Use `workspace` (default), `project`, `global`, or an explicit selector such as `workspace:team.api`, `project:alpha`, or `task:<id>`.")),
			mcp.WithString("kind", mcp.Description("Optional memory kind such as `decision`, `fact`, `constraint`, `handoff`, or `preference`. Defaults to `fact`.")),
			mcp.WithString("subject", mcp.Description("Optional short subject/title for this memory.")),
			mcp.WithArray("tags",
				mcp.Description("Optional string tags. Matching is case-insensitive."),
				mcp.WithStringItems(),
			),
			mcp.WithArray("supersedes_ids",
				mcp.Description("Optional prior memory IDs that this new memory supersedes."),
				mcp.WithStringItems(),
			),
		),
		rememberMemoryHandler(client, configPath),
	)

	srv.AddTool(
		mcp.NewTool("supersede_memory",
			mcp.WithDescription("Store a replacement memory entry and explicitly supersede one or more older memories. This is a UX wrapper around remember_memory(..., supersedes_ids=[...])."),
			mcp.WithString("content", mcp.Required(), mcp.Description("Replacement durable memory content.")),
			mcp.WithArray("supersedes_ids", mcp.Required(),
				mcp.Description("One or more prior memory IDs that this new memory supersedes."),
				mcp.WithStringItems(),
			),
			mcp.WithString("scope", mcp.Description("Scope selector. Use `workspace` (default), `project`, `global`, or an explicit selector such as `workspace:team.api`, `project:alpha`, or `task:<id>`.")),
			mcp.WithString("kind", mcp.Description("Optional memory kind such as `decision`, `fact`, `constraint`, `handoff`, or `preference`. Defaults to `fact`.")),
			mcp.WithString("subject", mcp.Description("Optional short subject/title for this replacement memory.")),
			mcp.WithArray("tags",
				mcp.Description("Optional string tags. Matching is case-insensitive."),
				mcp.WithStringItems(),
			),
		),
		supersedeMemoryHandler(client, configPath),
	)

	srv.AddTool(
		mcp.NewTool("recall_memories",
			mcp.WithDescription("Recall durable memories stored in the ax daemon. When no scopes are provided, recalls from `global`, the current project, and the current workspace."),
			mcp.WithArray("scopes",
				mcp.Description("Optional scope selectors. Accepts aliases `global`, `project`, `workspace` or explicit selectors like `project:alpha`, `workspace:team.api`, `task:<id>`."),
				mcp.WithStringItems(),
			),
			mcp.WithString("kind", mcp.Description("Optional kind filter such as `decision`, `fact`, `constraint`, `handoff`, or `preference`.")),
			mcp.WithArray("tags",
				mcp.Description("Optional tag filter. Returns memories containing any requested tag."),
				mcp.WithStringItems(),
			),
			mcp.WithBoolean("include_superseded", mcp.Description("Include superseded memories in addition to currently active ones.")),
			mcp.WithNumber("limit", mcp.Description("Maximum number of memories to return. Defaults to 10.")),
		),
		recallMemoriesHandler(client, configPath),
	)

	srv.AddTool(
		mcp.NewTool("list_memories",
			mcp.WithDescription("Inspect durable memories stored in the ax daemon. Use this when you want to browse or audit memory state, including superseded entries when requested."),
			mcp.WithArray("scopes",
				mcp.Description("Optional scope selectors. Accepts aliases `global`, `project`, `workspace` or explicit selectors like `project:alpha`, `workspace:team.api`, `task:<id>`."),
				mcp.WithStringItems(),
			),
			mcp.WithString("kind", mcp.Description("Optional kind filter such as `decision`, `fact`, `constraint`, `handoff`, or `preference`.")),
			mcp.WithArray("tags",
				mcp.Description("Optional tag filter. Returns memories containing any requested tag."),
				mcp.WithStringItems(),
			),
			mcp.WithBoolean("include_superseded", mcp.Description("Include superseded memories in addition to currently active ones.")),
			mcp.WithNumber("limit", mcp.Description("Maximum number of memories to return. Defaults to 10.")),
		),
		listMemoriesHandler(client, configPath),
	)
}

func rememberMemoryHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		content, err := request.RequireString("content")
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}
		scope, err := resolveMemoryScopeSelector(client, configPath, request.GetString("scope", "workspace"))
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		entry, err := client.RememberMemory(
			scope,
			request.GetString("kind", ""),
			request.GetString("subject", ""),
			content,
			request.GetStringSlice("tags", nil),
			request.GetStringSlice("supersedes_ids", nil),
		)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to remember memory: %v", err)), nil
		}

		data, _ := json.MarshalIndent(entry, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func supersedeMemoryHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		content, err := request.RequireString("content")
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}
		supersedes := request.GetStringSlice("supersedes_ids", nil)
		if len(supersedes) == 0 {
			return mcp.NewToolResultError("supersedes_ids must contain at least one memory ID"), nil
		}
		scope, err := resolveMemoryScopeSelector(client, configPath, request.GetString("scope", "workspace"))
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}

		entry, err := client.RememberMemory(
			scope,
			request.GetString("kind", ""),
			request.GetString("subject", ""),
			content,
			request.GetStringSlice("tags", nil),
			supersedes,
		)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to supersede memory: %v", err)), nil
		}

		data, _ := json.MarshalIndent(entry, "", "  ")
		return mcp.NewToolResultText(string(data)), nil
	}
}

func recallMemoriesHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		return memoryQueryToolResult(client, configPath, request)
	}
}

func listMemoriesHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		return memoryQueryToolResult(client, configPath, request)
	}
}

func memoryQueryToolResult(client *DaemonClient, configPath string, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	scopes, err := resolveMemoryScopeSelectors(client, configPath, request.GetStringSlice("scopes", nil))
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	limit := int(request.GetFloat("limit", float64(axmemory.DefaultLimit)))
	memories, err := client.RecallMemories(
		scopes,
		request.GetString("kind", ""),
		request.GetStringSlice("tags", nil),
		request.GetBool("include_superseded", false),
		limit,
	)
	if err != nil {
		return mcp.NewToolResultError(fmt.Sprintf("Failed to recall memories: %v", err)), nil
	}

	payload := map[string]any{
		"scopes":   scopes,
		"count":    len(memories),
		"memories": memories,
	}
	data, _ := json.MarshalIndent(payload, "", "  ")
	return mcp.NewToolResultText(string(data)), nil
}

func resolveMemoryScopeSelectors(client *DaemonClient, configPath string, scopes []string) ([]string, error) {
	if len(scopes) == 0 {
		scopes = []string{"global", "project", "workspace"}
	}
	result := make([]string, 0, len(scopes))
	seen := make(map[string]struct{}, len(scopes))
	for _, raw := range scopes {
		scope, err := resolveMemoryScopeSelector(client, configPath, raw)
		if err != nil {
			return nil, err
		}
		if _, ok := seen[scope]; ok {
			continue
		}
		seen[scope] = struct{}{}
		result = append(result, scope)
	}
	return result, nil
}

func resolveMemoryScopeSelector(client *DaemonClient, configPath, raw string) (string, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" || strings.EqualFold(raw, "workspace") {
		return axmemory.WorkspaceScope(client.workspace), nil
	}
	if strings.EqualFold(raw, "global") {
		return axmemory.GlobalScope, nil
	}
	if strings.EqualFold(raw, "project") {
		scope, err := currentProjectMemoryScope(client, configPath)
		if err != nil {
			return "", err
		}
		return scope, nil
	}

	scope := axmemory.NormalizeScope(raw)
	switch {
	case scope == axmemory.GlobalScope:
		return scope, nil
	case strings.HasPrefix(scope, "project:"):
		return scope, nil
	case strings.HasPrefix(scope, "workspace:"):
		return scope, nil
	case strings.HasPrefix(scope, "task:"):
		return scope, nil
	default:
		return "", fmt.Errorf("invalid memory scope %q; use `workspace`, `project`, `global`, or an explicit selector", raw)
	}
}

func currentProjectMemoryScope(client *DaemonClient, configPath string) (string, error) {
	cfgPath, err := resolveToolConfigPath(client, configPath)
	if err != nil {
		return "", err
	}
	tree, err := config.LoadTree(cfgPath)
	if err != nil {
		return "", err
	}
	prefix, ok := findProjectPrefixForWorkspace(tree, client.workspace)
	if !ok {
		return "", fmt.Errorf("workspace %q not found in config tree %s", client.workspace, cfgPath)
	}
	return axmemory.ProjectScope(prefix), nil
}

func findProjectPrefixForWorkspace(node *config.ProjectNode, target string) (string, bool) {
	if node == nil {
		return "", false
	}
	if workspace.OrchestratorName(node.Prefix) == target {
		return node.Prefix, true
	}
	for _, ws := range node.Workspaces {
		if ws.MergedName == target {
			return node.Prefix, true
		}
	}
	for _, child := range node.Children {
		if prefix, ok := findProjectPrefixForWorkspace(child, target); ok {
			return prefix, true
		}
	}
	return "", false
}
