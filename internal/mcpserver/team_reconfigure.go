package mcpserver

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/workspace"
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

type teamReconfigureArgs struct {
	ExpectedRevision *int                          `json:"expected_revision,omitempty"`
	Changes          []types.TeamReconfigureChange `json:"changes,omitempty"`
	ReconcileMode    types.TeamReconcileMode       `json:"reconcile_mode,omitempty"`
}

type teamApplyResult struct {
	Ticket    types.TeamApplyTicket       `json:"ticket"`
	State     types.TeamReconfigureState  `json:"state"`
	Reconcile workspace.ReconcileReport   `json:"reconcile"`
}

func teamStateToolDefinition() mcp.Tool {
	return mcp.NewTool("get_team_state",
		mcp.WithDescription("Read the daemon-managed effective team state for experimental MCP team reconfiguration."),
	)
}

func teamDryRunToolDefinition() mcp.Tool {
	return mcp.NewTool("dry_run_team_reconfigure",
		mcp.WithDescription("Plan supported v1 team changes against the daemon-managed effective state without reconciling runtime."),
		mcp.WithNumber("expected_revision", mcp.Description("Optional optimistic-lock revision. Dry-run fails if the current team revision differs.")),
		mcp.WithArray("changes",
			mcp.Required(),
			mcp.Description("Ordered v1 team changes. Supported kinds: workspace, child, root_orchestrator."),
			mcp.MinItems(1),
			mcp.Items(teamReconfigureChangeSchema()),
		),
	)
}

func teamApplyToolDefinition() mcp.Tool {
	return mcp.NewTool("apply_team_reconfigure",
		mcp.WithDescription("Apply supported v1 team changes via the daemon-managed effective state and run the requested reconcile mode."),
		mcp.WithNumber("expected_revision", mcp.Description("Optional optimistic-lock revision. Apply fails if the current team revision differs.")),
		mcp.WithArray("changes",
			mcp.Required(),
			mcp.Description("Ordered v1 team changes. Supported kinds: workspace, child, root_orchestrator."),
			mcp.MinItems(1),
			mcp.Items(teamReconfigureChangeSchema()),
		),
		mcp.WithString("reconcile_mode",
			mcp.Description("Runtime reconcile mode. `artifacts_only` avoids disrupting existing sessions; `start_missing` may safely restart/recreate managed sessions."),
			mcp.Enum(string(types.TeamReconcileArtifactsOnly), string(types.TeamReconcileStartMissing)),
		),
	)
}

func getTeamStateHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		cfgPath, err := resolveBaseToolConfigPath(configPath)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to resolve ax config: %v", err)), nil
		}
		state, err := client.GetTeamState(cfgPath)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to read team state: %v", err)), nil
		}
		return jsonToolResult(state)
	}
}

func dryRunTeamReconfigureHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		cfgPath, err := resolveBaseToolConfigPath(configPath)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to resolve ax config: %v", err)), nil
		}
		args, err := bindTeamReconfigureArgs(request)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}
		plan, err := client.DryRunTeamReconfigure(cfgPath, args.ExpectedRevision, args.Changes)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to plan team reconfiguration: %v", err)), nil
		}
		return jsonToolResult(plan)
	}
}

func applyTeamReconfigureHandler(client *DaemonClient, configPath string) server.ToolHandlerFunc {
	return func(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		cfgPath, err := resolveBaseToolConfigPath(configPath)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to resolve ax config: %v", err)), nil
		}
		args, err := bindTeamReconfigureArgs(request)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}
		if args.ReconcileMode == "" {
			args.ReconcileMode = types.TeamReconcileArtifactsOnly
		}

		ticket, err := client.ApplyTeamReconfigure(cfgPath, args.ExpectedRevision, args.Changes, args.ReconcileMode)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to begin team reconfiguration: %v", err)), nil
		}

		report, reconcileErr := reconcileAppliedTeam(ticket, client)
		actions := teamActionsFromReconcileReport(report)
		if reconcileErr != nil {
			if _, finishErr := client.FinishTeamReconfigure(ticket.Token, false, reconcileErr.Error(), actions); finishErr != nil {
				return mcp.NewToolResultError(fmt.Sprintf("Failed to finalize failed team reconfiguration %q: reconcile error=%v, finalize error=%v", ticket.Token, reconcileErr, finishErr)), nil
			}
			return mcp.NewToolResultError(fmt.Sprintf("Team reconfiguration %q failed during reconcile: %v", ticket.Token, reconcileErr)), nil
		}

		state, err := client.FinishTeamReconfigure(ticket.Token, true, "", actions)
		if err != nil {
			return mcp.NewToolResultError(fmt.Sprintf("Failed to finalize team reconfiguration %q: %v", ticket.Token, err)), nil
		}

		return jsonToolResult(teamApplyResult{
			Ticket:    *ticket,
			State:     *state,
			Reconcile: report,
		})
	}
}

func bindTeamReconfigureArgs(request mcp.CallToolRequest) (*teamReconfigureArgs, error) {
	var args teamReconfigureArgs
	if err := request.BindArguments(&args); err != nil {
		return nil, fmt.Errorf("Invalid team reconfigure arguments: %w", err)
	}
	if len(args.Changes) == 0 {
		return nil, fmt.Errorf("changes must contain at least one entry")
	}
	return &args, nil
}

func reconcileAppliedTeam(ticket *types.TeamApplyTicket, client *DaemonClient) (workspace.ReconcileReport, error) {
	var zero workspace.ReconcileReport
	if ticket == nil {
		return zero, fmt.Errorf("nil team apply ticket")
	}

	effectivePath := strings.TrimSpace(ticket.Plan.State.EffectiveConfigPath)
	if effectivePath == "" {
		effectivePath = strings.TrimSpace(ticket.Plan.State.BaseConfigPath)
	}
	if effectivePath == "" {
		return zero, fmt.Errorf("team apply ticket is missing an effective config path")
	}

	cfg, err := config.Load(effectivePath)
	if err != nil {
		return zero, fmt.Errorf("load effective config: %w", err)
	}
	tree, err := config.LoadTree(effectivePath)
	if err != nil {
		return zero, fmt.Errorf("load effective config tree: %w", err)
	}

	includeRoot := tree == nil || !tree.DisableRootOrchestrator
	desired, err := workspace.BuildDesiredState(cfg, tree, client.socketPath, effectivePath, includeRoot)
	if err != nil {
		return zero, fmt.Errorf("build desired runtime state: %w", err)
	}

	reconciler := workspace.NewReconciler(client.socketPath, effectivePath)
	report, err := reconciler.ReconcileDesiredState(desired, workspace.ReconcileOptions{
		DaemonRunning:          ticket.ReconcileMode == types.TeamReconcileStartMissing,
		AllowDisruptiveChanges: ticket.ReconcileMode == types.TeamReconcileStartMissing,
	})
	if err != nil {
		return report, err
	}
	return report, nil
}

func teamActionsFromReconcileReport(report workspace.ReconcileReport) []types.TeamReconfigureAction {
	actions := make([]types.TeamReconfigureAction, 0, len(report.Actions)+1)
	for _, action := range report.Actions {
		kind, ok := reconcileActionKind(action.Kind)
		if !ok {
			continue
		}
		actions = append(actions, types.TeamReconfigureAction{
			Action: action.Operation,
			Kind:   kind,
			Name:   action.Name,
			Detail: action.Details,
		})
	}
	if report.RootManualRestartRequired {
		actions = append(actions, types.TeamReconfigureAction{
			Action: "manual_restart_required",
			Kind:   types.TeamEntryRootOrchestrator,
			Name:   "orchestrator",
			Detail: strings.Join(report.RootManualRestartReasons, "; "),
		})
	}
	return actions
}

func reconcileActionKind(value string) (types.TeamEntryKind, bool) {
	switch strings.TrimSpace(value) {
	case "workspace":
		return types.TeamEntryWorkspace, true
	case "orchestrator":
		return types.TeamEntryRootOrchestrator, true
	default:
		return "", false
	}
}

func jsonToolResult(value any) (*mcp.CallToolResult, error) {
	data, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return mcp.NewToolResultError(fmt.Sprintf("Failed to encode result: %v", err)), nil
	}
	return mcp.NewToolResultText(string(data)), nil
}

func teamReconfigureChangeSchema() map[string]any {
	return map[string]any{
		"type": "object",
		"properties": map[string]any{
			"op": map[string]any{
				"type":        "string",
				"description": "Operation kind.",
				"enum": []string{
					string(types.TeamChangeAdd),
					string(types.TeamChangeRemove),
					string(types.TeamChangeEnable),
					string(types.TeamChangeDisable),
				},
			},
			"kind": map[string]any{
				"type":        "string",
				"description": "Target entry kind.",
				"enum": []string{
					string(types.TeamEntryWorkspace),
					string(types.TeamEntryChild),
					string(types.TeamEntryRootOrchestrator),
				},
			},
			"name": map[string]any{
				"type":        "string",
				"description": "Workspace or child name. Omit for root_orchestrator changes.",
			},
			"workspace": map[string]any{
				"type":        "object",
				"description": "Workspace spec for workspace add operations.",
				"properties": map[string]any{
					"dir": map[string]any{"type": "string"},
					"description": map[string]any{"type": "string"},
					"shell": map[string]any{"type": "string"},
					"runtime": map[string]any{"type": "string"},
					"codex_model_reasoning_effort": map[string]any{"type": "string"},
					"agent": map[string]any{"type": "string"},
					"instructions": map[string]any{"type": "string"},
					"env": map[string]any{
						"type":                 "object",
						"additionalProperties": map[string]any{"type": "string"},
					},
				},
			},
			"child": map[string]any{
				"type":        "object",
				"description": "Child spec for child add operations.",
				"properties": map[string]any{
					"dir":    map[string]any{"type": "string"},
					"prefix": map[string]any{"type": "string"},
				},
			},
		},
		"required":             []string{"op", "kind"},
		"additionalProperties": false,
	}
}
