//! MCP server scaffold + tool registrations that delegate to the
//! daemon client. Groups covered: shared, workspace, memory, messages,
//! usage, tasks, agents, and `team_reconfigure`.
//!
//! Each tool body calls into the `DaemonClient` using the typed
//! envelope payloads and returns JSON-formatted text through
//! `CallToolResult::success`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::service::RunningService;
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;

use ax_config::{Config, ProjectNode};
use ax_proto::payloads::{
    AgentLifecyclePayload, BroadcastPayload, CancelTaskPayload, CreateTaskPayload,
    FinishTeamReconfigurePayload, GetSharedPayload, GetTaskPayload, GetTeamStatePayload,
    InterveneTaskPayload, ListTasksPayload, ReadMessagesPayload, RecallMemoriesPayload,
    RecordMcpToolActivityPayload, RememberMemoryPayload, RemoveTaskPayload, SendMessagePayload,
    SetSharedPayload, SetStatusPayload, StartTaskPayload, TeamReconfigurePayload,
    UpdateTaskPayload, UsageTrendWorkspace, UsageTrendsPayload,
};
use ax_proto::responses::{
    AgentLifecycleResponse, BroadcastResponse, GetSharedResponse, InterveneTaskResponse,
    ListSharedResponse, ListTasksResponse, ListWorkspacesResponse, MemoryResponse,
    ReadMessagesResponse, RecallMemoriesResponse, SendMessageResponse, StartTaskResponse,
    StatusResponse, TaskResponse, TeamApplyResponse, TeamPlanResponse, TeamStateResponse,
    UsageTrendsResponse,
};
use ax_proto::types::{
    AgentStatus, LifecycleAction, McpToolActivityStatus, TaskStatus, TeamApplyTicket, TeamChangeOp,
    TeamChildSpec, TeamEntryKind, TeamReconcileMode, TeamReconfigureAction, TeamReconfigureChange,
    TeamWorkspaceSpec, WorkspaceInfo,
};
use ax_proto::MessageType;
use ax_workspace::{build_desired_state_with_tree, ReconcileOptions, ReconcileReport, Reconciler};

use crate::daemon_client::{DaemonClient, DaemonClientError};
use crate::memory_scope;
use crate::telemetry::{TelemetryEvent, TelemetrySink};

const MCP_ACTIVITY_FIELD_LIMIT: usize = 240;

/// Entry point for `ax-cli mcp-server`: connect the daemon client,
/// hand it to the MCP server, and run the stdio transport loop. The
/// returned future completes when the peer closes the transport or
/// the server's shutdown token fires.
pub async fn run_stdio(server: Server) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service: RunningService<_, _> = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// MCP server that exposes the ax daemon as a set of tools. Holds
/// the daemon client, an optional effective config path used by the
/// memory scope resolver + message dispatch, and the macro-generated
/// tool router. Clone is cheap because every field is behind an
/// `Arc`.
#[derive(Clone)]
pub struct Server {
    daemon: DaemonClient,
    config_path: Option<PathBuf>,
    tool_router: ToolRouter<Self>,
    telemetry: Option<Arc<TelemetrySink>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("workspace", &self.daemon.workspace())
            .field("socket_path", &self.daemon.socket_path())
            .finish_non_exhaustive()
    }
}

impl Server {
    #[must_use]
    pub fn new(daemon: DaemonClient) -> Self {
        Self {
            daemon,
            config_path: None,
            tool_router: Self::tool_router(),
            telemetry: None,
        }
    }

    /// Attach a telemetry sink. Every tool call routed through the
    /// MCP transport emits one append-only record; direct method
    /// calls (used by unit tests) bypass telemetry by design.
    #[must_use]
    pub fn with_telemetry(mut self, sink: TelemetrySink) -> Self {
        self.telemetry = Some(Arc::new(sink));
        self
    }

    /// Write one tool-call record to the attached sink, if any. Kept
    /// crate-visible so the `call_tool` override and future harness
    /// tests can share the same recording path.
    pub(crate) fn record_tool_call(&self, tool: &str, ok: bool, duration_ms: u64, err_kind: &str) {
        let Some(sink) = self.telemetry.as_ref() else {
            return;
        };
        sink.record(&TelemetryEvent {
            ts: chrono::Utc::now(),
            workspace: self.daemon.workspace().to_owned(),
            tool: tool.to_owned(),
            ok,
            duration_ms,
            err_kind: err_kind.to_owned(),
        });
    }

    /// Best-effort user-visible activity emission through the daemon.
    /// This deliberately records only sanitized routing/outcome data,
    /// never raw tool arguments or result content.
    pub(crate) fn record_mcp_tool_activity(
        &self,
        tool: &str,
        ok: bool,
        duration_ms: u64,
        err_kind: &str,
    ) {
        let daemon = self.daemon.clone();
        let payload = RecordMcpToolActivityPayload {
            tool: sanitize_mcp_activity_field(tool),
            task_id: String::new(),
            status: if ok {
                McpToolActivityStatus::Ok
            } else {
                McpToolActivityStatus::Error
            },
            error_kind: if ok {
                String::new()
            } else {
                sanitize_mcp_activity_field(err_kind)
            },
            duration_ms: i64::try_from(duration_ms).unwrap_or(i64::MAX),
        };

        tokio::spawn(async move {
            let result: Result<StatusResponse, DaemonClientError> = daemon
                .request(MessageType::RecordMcpToolActivity, &payload)
                .await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "MCP tool activity write failed");
            }
        });
    }

    /// Record the base `.ax/config.yaml` the peer is operating
    /// against. Tools use this when resolving project memory scope
    /// and when stamping `dispatch_config_path` on outgoing messages.
    #[must_use]
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }

    #[must_use]
    pub fn daemon(&self) -> &DaemonClient {
        &self.daemon
    }

    #[must_use]
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// Resolve the effective config path for tools that dispatch
    /// through the daemon (`send_message` / `broadcast` /
    /// project-scope memory). Returns whatever was passed to
    /// [`Self::with_config_path`]; callers who want a CWD-based
    /// fallback should resolve the path at the binary entry point
    /// and pass it explicitly so tests can keep the process-global
    /// env untouched.
    fn effective_config(&self) -> Option<PathBuf> {
        self.config_path.clone()
    }

    fn instructions(&self) -> String {
        format!(
            "You are the {:?} workspace agent in an ax multi-agent environment. \
             Use these tools to coordinate with other workspace agents. \
             Call list_agents to inspect configured agents from the active ax config, \
             call list_workspaces to see who is currently active, and read_messages \
             periodically to check for incoming messages from other agents.",
            self.daemon.workspace()
        )
    }
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct SetSharedValueRequest {
    /// Key name.
    pub key: String,
    /// Value to store.
    pub value: String,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct GetSharedValueRequest {
    /// Key name to look up.
    pub key: String,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct SetStatusRequest {
    /// Status text describing current activity.
    pub status: String,
}

fn default_memory_limit() -> i64 {
    10
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct RememberMemoryRequest {
    /// Durable memory content to persist.
    pub content: String,
    /// Scope selector. Use `workspace` (default), `project`,
    /// `global`, or an explicit selector such as
    /// `workspace:team.api`, `project:alpha`, or `task:<id>`.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional memory kind such as `decision`, `fact`, `constraint`,
    /// `handoff`, or `preference`. Defaults to `fact`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Optional short subject/title for this memory.
    #[serde(default)]
    pub subject: Option<String>,
    /// Optional string tags. Matching is case-insensitive.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional prior memory IDs that this new memory supersedes.
    #[serde(default)]
    pub supersedes_ids: Vec<String>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct SupersedeMemoryRequest {
    /// Replacement durable memory content.
    pub content: String,
    /// One or more prior memory IDs that this new memory supersedes.
    pub supersedes_ids: Vec<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct MemoryQueryRequest {
    /// Optional scope selectors. Accepts aliases `global`, `project`,
    /// `workspace` or explicit selectors. Empty defaults to
    /// `[global, project, workspace]`.
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Include superseded memories in addition to active ones.
    #[serde(default)]
    pub include_superseded: bool,
    /// Maximum number of memories to return. Defaults to 10.
    #[serde(default = "default_memory_limit")]
    pub limit: i64,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct SendMessageRequest {
    /// Target workspace name.
    pub to: String,
    /// Message content to send.
    pub message: String,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct ReadMessagesRequest {
    /// Max number of messages to read (default: 10).
    #[serde(default)]
    pub limit: Option<i64>,
    /// Filter messages from a specific workspace.
    #[serde(default)]
    pub from: Option<String>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct BroadcastRequest {
    /// Message to broadcast.
    pub message: String,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct CreateTaskRequest {
    /// Short task title.
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Workspace to assign the task to.
    pub assignee: String,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    /// `default` (session reuse) or `fresh` (recreate session before
    /// the first task-aware dispatch).
    #[serde(default)]
    pub start_mode: Option<String>,
    /// `parallel` (default) or `serial`.
    #[serde(default)]
    pub workflow_mode: Option<String>,
    /// `low`, `normal`, `high`, `urgent`. Defaults to `normal`.
    #[serde(default)]
    pub priority: Option<String>,
    /// When >0, marks the task stale if no progress update arrives
    /// within this many seconds while pending/`in_progress`.
    #[serde(default)]
    pub stale_after_seconds: Option<i64>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct StartTaskRequest {
    pub title: String,
    /// Initial dispatch message sent to the assignee. `Task ID:` is
    /// prepended automatically by the daemon.
    pub message: String,
    #[serde(default)]
    pub description: Option<String>,
    pub assignee: String,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub start_mode: Option<String>,
    #[serde(default)]
    pub workflow_mode: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub stale_after_seconds: Option<i64>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct UpdateTaskRequest {
    pub id: String,
    /// New status: pending, `in_progress`, completed, or failed.
    #[serde(default)]
    pub status: Option<String>,
    /// Task result summary (typically set on completion).
    #[serde(default)]
    pub result: Option<String>,
    /// Progress log entry to append.
    #[serde(default)]
    pub log: Option<String>,
    /// When setting status to `completed`, pass `confirm: true` AFTER
    /// self-verifying the Completion Reporting Contract checklist
    /// (result format, leftover scope, evidence). The daemon rejects
    /// unconfirmed completions with the checklist inline so you can
    /// re-call with `confirm: true` once you've checked.
    #[serde(default)]
    pub confirm: Option<bool>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct ReportTaskCompletionRequest {
    pub id: String,
    /// Human-readable summary of what was done.
    pub summary: String,
    /// Owned files that are still dirty (uncommitted / out-of-place).
    /// Use an empty list if nothing is left over.
    #[serde(default)]
    pub dirty_files: Vec<String>,
    /// Optional description of residual work if `dirty_files` is
    /// non-empty. Required by the Completion Reporting Contract when
    /// there is leftover scope; otherwise ignored.
    #[serde(default)]
    pub residual_scope: Option<String>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct TaskIdRequest {
    pub id: String,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct ListTasksRequest {
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub created_by: Option<String>,
    /// pending, `in_progress`, completed, failed, cancelled.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct ListWorkspaceTasksRequest {
    pub workspace: String,
    /// `assigned`, `created`, or `both` (default).
    #[serde(default)]
    pub view: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct ControlTaskRequest {
    pub id: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// Optional optimistic-concurrency guard.
    #[serde(default)]
    pub expected_version: Option<i64>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct InterveneTaskRequest {
    pub id: String,
    /// Bounded action: `wake`, `interrupt`, or `retry`.
    pub action: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub expected_version: Option<i64>,
}

#[derive(Debug, Default, schemars::JsonSchema, Deserialize)]
pub struct ListAgentsRequest {
    /// Case-insensitive search applied to name, description, runtime,
    /// command, and instructions preview.
    #[serde(default)]
    pub query: Option<String>,
    /// Return only agents currently registered with the daemon.
    #[serde(default)]
    pub active_only: bool,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct AgentNameRequest {
    /// Configured workspace / managed child orchestrator name.
    pub name: String,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct InterruptAgentRequest {
    /// Target workspace name.
    pub name: String,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct SendKeysRequest {
    /// Target workspace name.
    pub workspace: String,
    /// Ordered key sequence. Named tmux keys (Enter, Escape, `C-c`, …)
    /// are resolved as-is; anything else is typed literally.
    pub keys: Vec<String>,
}

const DEFAULT_INSPECT_QUESTION: &str = "현재 운영 상태를 간단히 요약해줘. 담당 역할, 최근 작업, 대기 중인 이슈, 핵심 지표나 리스크가 있으면 함께 알려줘. 포트폴리오 개발팀을 맡고 있다면 담당 서비스나 기능, 최근 변경사항, 남은 개발 과제, 배포 또는 운영 리스크를 짧게 포함해줘.";

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct InspectAgentRequest {
    /// Configured agent/workspace name.
    pub name: String,
    /// Optional custom question. Defaults to asking for current
    /// operating status, recent work, risks, and key metrics.
    #[serde(default)]
    pub question: Option<String>,
    /// Max seconds to wait for reply. Defaults to 120.
    #[serde(default)]
    pub timeout: Option<i64>,
}

#[derive(Debug, schemars::JsonSchema, Deserialize)]
pub struct RequestMessageRequest {
    /// Target workspace name.
    pub to: String,
    /// Task or question to send.
    pub message: String,
    /// Max seconds to wait for reply. Defaults to 120.
    #[serde(default)]
    pub timeout: Option<i64>,
}

#[derive(Debug, Default, schemars::JsonSchema, Deserialize)]
pub struct UsageTrendsRequest {
    /// Optional workspace name filter. When omitted, queries all
    /// active workspaces.
    #[serde(default)]
    pub workspace: Option<String>,
    /// History window in minutes. Defaults to 180.
    #[serde(default)]
    pub since_minutes: Option<i64>,
    /// Bucket size in minutes. Defaults to 5.
    #[serde(default)]
    pub bucket_minutes: Option<i64>,
}

#[derive(Debug, Default, schemars::JsonSchema, Deserialize)]
pub struct PlanInitialTeamRequest {
    /// Absolute path to the project root to survey. Defaults to the
    /// MCP server's current working directory.
    #[serde(default)]
    pub project_dir: Option<String>,
}

#[derive(Debug, Clone, Default, schemars::JsonSchema, Deserialize)]
pub struct PlanTeamReconfigureRequest {
    /// Absolute path to the project root. Defaults to the parent
    /// of the server's effective config path (`.ax/` parent).
    #[serde(default)]
    pub project_dir: Option<String>,
}

#[derive(Debug, Clone, Default, schemars::JsonSchema, Deserialize)]
pub struct TeamReconfigureRequest {
    /// Optional optimistic-lock revision.
    #[serde(default)]
    pub expected_revision: Option<i64>,
    /// Ordered v1 team changes. Supported kinds: `workspace`, `child`,
    /// `root_orchestrator`.
    pub changes: Vec<TeamReconfigureChangeInput>,
    /// Runtime reconcile mode: `artifacts_only` (default) or
    /// `start_missing`.
    #[serde(default)]
    pub reconcile_mode: Option<String>,
}

#[derive(Debug, Clone, schemars::JsonSchema, Deserialize)]
pub struct TeamReconfigureChangeInput {
    /// Operation kind: `add`, `remove`, `enable`, or `disable`.
    pub op: String,
    /// Target entry kind: `workspace`, `child`, or `root_orchestrator`.
    pub kind: String,
    /// Workspace or child name. Omit for `root_orchestrator` changes.
    #[serde(default)]
    pub name: Option<String>,
    /// Workspace spec for `workspace` add operations.
    #[serde(default)]
    pub workspace: Option<TeamWorkspaceSpecInput>,
    /// Child spec for `child` add operations.
    #[serde(default)]
    pub child: Option<TeamChildSpecInput>,
}

#[derive(Debug, Clone, Default, schemars::JsonSchema, Deserialize)]
pub struct TeamWorkspaceSpecInput {
    pub dir: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub codex_model_reasoning_effort: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, schemars::JsonSchema, Deserialize)]
pub struct TeamChildSpecInput {
    pub dir: String,
    #[serde(default)]
    pub prefix: Option<String>,
}

#[tool_router(router = tool_router)]
impl Server {
    /// `set_shared_value` — store a key-value pair visible to every
    /// workspace agent connected to the daemon.
    #[tool(
        description = "Store a key-value pair visible to all workspace agents. Useful for sharing API endpoints, config, decisions."
    )]
    pub async fn set_shared_value(
        &self,
        Parameters(req): Parameters<SetSharedValueRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let _: StatusResponse = self
            .daemon
            .request(
                MessageType::SetShared,
                &SetSharedPayload {
                    key: req.key.clone(),
                    value: req.value.clone(),
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::json!({
                "ok": true,
                "key": req.key,
            })
            .to_string(),
        )]))
    }

    /// `get_shared_value` — read a shared pair any workspace agent set.
    #[tool(description = "Read a shared key-value pair set by any workspace agent.")]
    pub async fn get_shared_value(
        &self,
        Parameters(req): Parameters<GetSharedValueRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let resp: GetSharedResponse = self
            .daemon
            .request(
                MessageType::GetShared,
                &GetSharedPayload {
                    key: req.key.clone(),
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "key": resp.key,
                "value": resp.value,
                "found": resp.found,
            }))
            .unwrap_or_default(),
        )]))
    }

    /// `list_shared_values` — return every shared pair.
    #[tool(description = "List all shared key-value pairs across workspaces.")]
    pub async fn list_shared_values(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let resp: ListSharedResponse = self
            .daemon
            .request(MessageType::ListShared, &serde_json::json!({}))
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.values).unwrap_or_default(),
        )]))
    }

    /// `list_workspaces` — active agents from the registry.
    #[tool(description = "List all active workspace agents and their current status.")]
    pub async fn list_workspaces(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let resp: ListWorkspacesResponse = self
            .daemon
            .request(MessageType::ListWorkspaces, &serde_json::json!({}))
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "workspace": self.daemon.workspace(),
                "count": resp.workspaces.len(),
                "workspaces": resp.workspaces,
            }))
            .unwrap_or_default(),
        )]))
    }

    /// `set_status` — free-form status text for the caller workspace.
    #[tool(
        description = "Update your workspace status text, visible to other agents via list_workspaces."
    )]
    pub async fn set_status(
        &self,
        Parameters(req): Parameters<SetStatusRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let _: StatusResponse = self
            .daemon
            .request(
                MessageType::SetStatus,
                &SetStatusPayload {
                    status: req.status.clone(),
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::json!({
                "ok": true,
                "status": req.status,
            })
            .to_string(),
        )]))
    }

    /// `remember_memory` — persist a durable memory entry.
    #[tool(
        description = "Persist a durable project/workspace memory in the ax daemon so it survives runtime restarts and tool changes. Use for lasting decisions, facts, constraints, handoffs, and preferences."
    )]
    pub async fn remember_memory(
        &self,
        Parameters(req): Parameters<RememberMemoryRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let scope_raw = req.scope.as_deref().unwrap_or("workspace");
        let scope = memory_scope::resolve(
            scope_raw,
            self.daemon.workspace(),
            self.effective_config().as_deref(),
        )
        .map_err(|e| rmcp::ErrorData::invalid_params(e.to_string(), None))?;
        let resp: MemoryResponse = self
            .daemon
            .request(
                MessageType::RememberMemory,
                &RememberMemoryPayload {
                    scope,
                    kind: req.kind.clone().unwrap_or_default(),
                    subject: req.subject.clone().unwrap_or_default(),
                    content: req.content.clone(),
                    tags: req.tags.clone(),
                    supersedes: req.supersedes_ids.clone(),
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.memory).unwrap_or_default(),
        )]))
    }

    /// `supersede_memory` — thin wrapper around `remember_memory`
    /// that requires at least one `supersedes_ids` entry.
    #[tool(
        description = "Store a replacement memory entry and explicitly supersede one or more older memories. This is a UX wrapper around remember_memory(..., supersedes_ids=[...])."
    )]
    pub async fn supersede_memory(
        &self,
        Parameters(req): Parameters<SupersedeMemoryRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if req.supersedes_ids.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "supersedes_ids must contain at least one memory ID",
                None,
            ));
        }
        let scope_raw = req.scope.as_deref().unwrap_or("workspace");
        let scope = memory_scope::resolve(
            scope_raw,
            self.daemon.workspace(),
            self.effective_config().as_deref(),
        )
        .map_err(|e| rmcp::ErrorData::invalid_params(e.to_string(), None))?;
        let resp: MemoryResponse = self
            .daemon
            .request(
                MessageType::RememberMemory,
                &RememberMemoryPayload {
                    scope,
                    kind: req.kind.clone().unwrap_or_default(),
                    subject: req.subject.clone().unwrap_or_default(),
                    content: req.content.clone(),
                    tags: req.tags.clone(),
                    supersedes: req.supersedes_ids.clone(),
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.memory).unwrap_or_default(),
        )]))
    }

    /// `recall_memories` — read durable memories filtered by scope
    /// and kind. Defaults pull from `[global, project, workspace]`.
    #[tool(
        description = "Recall durable memories stored in the ax daemon. When no scopes are provided, recalls from `global`, the current project, and the current workspace."
    )]
    pub async fn recall_memories(
        &self,
        Parameters(req): Parameters<MemoryQueryRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.memory_query(req).await
    }

    /// `list_memories` — identical plumbing to `recall_memories`;
    /// kept as a distinct tool so UI clients can choose a semantic
    /// name matching browse/audit intent.
    #[tool(
        description = "Inspect durable memories stored in the ax daemon. Use this when you want to browse or audit memory state, including superseded entries when requested."
    )]
    pub async fn list_memories(
        &self,
        Parameters(req): Parameters<MemoryQueryRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.memory_query(req).await
    }

    /// `send_message` — deliver a message to another workspace via
    /// the daemon's queue, with optional `dispatch_config_path` so
    /// the wake scheduler can ensure the recipient's session.
    #[tool(
        description = "Send a message to another workspace agent. Use this to coordinate with other agents working on the same project."
    )]
    pub async fn send_message(
        &self,
        Parameters(req): Parameters<SendMessageRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let dispatch_path = self
            .effective_config()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let resp: SendMessageResponse = self
            .daemon
            .request(
                MessageType::SendMessage,
                &SendMessagePayload {
                    to: req.to.clone(),
                    message: req.message.clone(),
                    config_path: dispatch_path,
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Message sent to {:?} (id: {})",
            req.to, resp.message_id
        ))]))
    }

    /// `read_messages` — drain pending messages from the caller's
    /// inbox; optional sender filter and limit (default 10).
    #[tool(
        description = "Read pending messages from other workspace agents. Call this periodically to check for incoming coordination messages."
    )]
    pub async fn read_messages(
        &self,
        Parameters(req): Parameters<ReadMessagesRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use std::fmt::Write as _;

        let limit = req.limit.unwrap_or(10);
        let from = req.from.clone().unwrap_or_default();

        let resp: ReadMessagesResponse = self
            .daemon
            .request(
                MessageType::ReadMessages,
                &ReadMessagesPayload { limit, from },
            )
            .await
            .map_err(tool_error)?;

        if resp.messages.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No pending messages.",
            )]));
        }
        let mut out = String::new();
        let _ = writeln!(out, "{} message(s):\n", resp.messages.len());
        for msg in &resp.messages {
            let _ = write!(
                out,
                "From: {}\nTime: {}\n{}\n\n---\n\n",
                msg.from,
                msg.created_at.format("%H:%M:%S"),
                msg.content,
            );
        }
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    /// `create_task` — record a pending task without dispatching it.
    #[tool(
        description = "Create a task record and assign it to a workspace agent without dispatching it. Use start_task when work should begin immediately."
    )]
    pub async fn create_task(
        &self,
        Parameters(req): Parameters<CreateTaskRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        validate_lifecycle_options(
            req.start_mode.as_deref(),
            req.workflow_mode.as_deref(),
            req.priority.as_deref(),
            req.stale_after_seconds,
        )?;
        let payload = CreateTaskPayload {
            title: req.title,
            description: req.description.unwrap_or_default(),
            assignee: req.assignee,
            parent_task_id: req.parent_task_id.unwrap_or_default(),
            start_mode: req.start_mode.unwrap_or_default(),
            workflow_mode: req.workflow_mode.unwrap_or_default(),
            priority: req.priority.unwrap_or_default(),
            stale_after_seconds: req.stale_after_seconds.unwrap_or(0),
        };
        let resp: TaskResponse = self
            .daemon
            .request(MessageType::CreateTask, &payload)
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.task).unwrap_or_default(),
        )]))
    }

    /// `start_task` — create a task and let the daemon handle the
    /// initial dispatch. Serial workflow children may come back with
    /// `dispatch.status = "waiting_turn"`.
    #[tool(
        description = "Create a task and let the daemon persist or release the initial task-aware dispatch. Prefer this over create_task + send_message when work should begin immediately; serial workflow children may return `dispatch.status=\"waiting_turn\"` until prior siblings become terminal."
    )]
    pub async fn start_task(
        &self,
        Parameters(req): Parameters<StartTaskRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        validate_lifecycle_options(
            req.start_mode.as_deref(),
            req.workflow_mode.as_deref(),
            req.priority.as_deref(),
            req.stale_after_seconds,
        )?;
        let payload = StartTaskPayload {
            title: req.title,
            description: req.description.unwrap_or_default(),
            message: req.message,
            assignee: req.assignee,
            parent_task_id: req.parent_task_id.unwrap_or_default(),
            start_mode: req.start_mode.unwrap_or_default(),
            workflow_mode: req.workflow_mode.unwrap_or_default(),
            priority: req.priority.unwrap_or_default(),
            stale_after_seconds: req.stale_after_seconds.unwrap_or(0),
        };
        let resp: StartTaskResponse =
            match self.daemon.request(MessageType::StartTask, &payload).await {
                Ok(resp) => resp,
                Err(e) => return Ok(daemon_tool_execution_error(e)),
            };
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_default(),
        )]))
    }

    /// `update_task` — set status, result, or append a progress log.
    #[tool(description = "Update a task's status, result, or append a progress log entry.")]
    pub async fn update_task(
        &self,
        Parameters(req): Parameters<UpdateTaskRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let status = match req.status.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(s) => {
                Some(parse_update_status(s).map_err(|e| rmcp::ErrorData::invalid_params(e, None))?)
            }
        };
        let payload = UpdateTaskPayload {
            id: req.id,
            status,
            result: req.result.filter(|s| !s.is_empty()),
            log: req.log.filter(|s| !s.is_empty()),
            confirm: req.confirm,
        };
        let resp: TaskResponse = self
            .daemon
            .request(MessageType::UpdateTask, &payload)
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.task).unwrap_or_default(),
        )]))
    }

    /// `report_task_completion` — ergonomic wrapper over `update_task`
    /// that constructs the Completion Reporting Contract marker from
    /// structured fields. Any MCP-speaking agent can call this
    /// without memorising the exact marker string, which is the
    /// dominant source of silent completion rejections.
    #[tool(
        description = "Report task completion with structured fields. Constructs the Completion Reporting Contract marker from `dirty_files` and `residual_scope` for you, then transitions the task to completed. Use this instead of `update_task` whenever you are closing out a task — it is the lowest-friction path."
    )]
    pub async fn report_task_completion(
        &self,
        Parameters(req): Parameters<ReportTaskCompletionRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let summary = req.summary.trim();
        if summary.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "summary is required",
                None,
            ));
        }
        let clean_files: Vec<String> = req
            .dirty_files
            .into_iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let marker = if clean_files.is_empty() {
            "remaining owned dirty files=<none>".to_owned()
        } else {
            let residual = req
                .residual_scope
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    rmcp::ErrorData::invalid_params(
                        "residual_scope is required when dirty_files is non-empty",
                        None,
                    )
                })?;
            format!(
                "remaining owned dirty files={}; residual scope={}",
                clean_files.join(", "),
                residual,
            )
        };
        let result = format!("{summary}\n\n{marker}");
        let payload = UpdateTaskPayload {
            id: req.id,
            status: Some(TaskStatus::Completed),
            result: Some(result),
            log: None,
            confirm: Some(true),
        };
        let resp: TaskResponse = self
            .daemon
            .request(MessageType::UpdateTask, &payload)
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.task).unwrap_or_default(),
        )]))
    }

    /// `get_task` — single-task inspect view including logs + rollup.
    #[tool(description = "Get detailed information about a specific task including its logs.")]
    pub async fn get_task(
        &self,
        Parameters(req): Parameters<TaskIdRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let resp: TaskResponse = self
            .daemon
            .request(MessageType::GetTask, &GetTaskPayload { id: req.id })
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.task).unwrap_or_default(),
        )]))
    }

    /// `list_tasks` — raw task list with optional assignee / creator
    /// / status filters. Prefer `list_workspace_tasks` for
    /// workspace-centric views.
    #[tool(
        description = "List tasks with optional raw filters. Returns all tasks if no filters are specified. Prefer `list_workspace_tasks` when querying tasks relative to a workspace."
    )]
    pub async fn list_tasks(
        &self,
        Parameters(req): Parameters<ListTasksRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let status = parse_list_status(req.status.as_deref())
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        let payload = ListTasksPayload {
            assignee: req.assignee.unwrap_or_default(),
            created_by: req.created_by.unwrap_or_default(),
            status,
        };
        let resp: ListTasksResponse = self
            .daemon
            .request(MessageType::ListTasks, &payload)
            .await
            .map_err(tool_error)?;
        if resp.tasks.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No tasks found.",
            )]));
        }
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "count": resp.tasks.len(),
                "tasks": resp.tasks,
            }))
            .unwrap_or_default(),
        )]))
    }

    /// `list_workspace_tasks` — assigned / created / both views for
    /// one workspace. Aggregates via two daemon calls when `both`.
    #[tool(
        description = "List tasks relative to a workspace with an explicit view: tasks assigned to that workspace, tasks created by that workspace, or both."
    )]
    pub async fn list_workspace_tasks(
        &self,
        Parameters(req): Parameters<ListWorkspaceTasksRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let workspace = req.workspace.trim();
        if workspace.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "workspace is required",
                None,
            ));
        }
        let view = parse_workspace_view(req.view.as_deref())
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        let status = parse_list_status(req.status.as_deref())
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        let mut assigned_block = None;
        let mut created_block = None;
        let mut unique_ids = std::collections::BTreeSet::new();
        let status_label = status.as_ref().map(status_rename).unwrap_or_default();

        if matches!(view, WorkspaceView::Assigned | WorkspaceView::Both) {
            let resp: ListTasksResponse = self
                .daemon
                .request(
                    MessageType::ListTasks,
                    &ListTasksPayload {
                        assignee: workspace.to_owned(),
                        created_by: String::new(),
                        status: status.clone(),
                    },
                )
                .await
                .map_err(tool_error)?;
            for task in &resp.tasks {
                unique_ids.insert(task.id.clone());
            }
            assigned_block = Some(serde_json::json!({
                "count": resp.tasks.len(),
                "tasks": resp.tasks,
            }));
        }
        if matches!(view, WorkspaceView::Created | WorkspaceView::Both) {
            let resp: ListTasksResponse = self
                .daemon
                .request(
                    MessageType::ListTasks,
                    &ListTasksPayload {
                        assignee: String::new(),
                        created_by: workspace.to_owned(),
                        status: status.clone(),
                    },
                )
                .await
                .map_err(tool_error)?;
            for task in &resp.tasks {
                unique_ids.insert(task.id.clone());
            }
            created_block = Some(serde_json::json!({
                "count": resp.tasks.len(),
                "tasks": resp.tasks,
            }));
        }

        let mut body = serde_json::json!({
            "workspace": workspace,
            "view": view.as_str(),
            "unique_task_count": unique_ids.len(),
        });
        if !status_label.is_empty() {
            body.as_object_mut()
                .expect("body is object")
                .insert("status".to_owned(), serde_json::Value::String(status_label));
        }
        if let Some(block) = assigned_block {
            body.as_object_mut()
                .expect("body is object")
                .insert("assigned".to_owned(), block);
        }
        if let Some(block) = created_block {
            body.as_object_mut()
                .expect("body is object")
                .insert("created".to_owned(), block);
        }
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )]))
    }

    /// `cancel_task` — creators, assignees, and the CLI operator
    /// may cancel pending/in_progress tasks.
    #[tool(
        description = "Cancel a task via a dedicated control path. Creators, assignees, and the CLI operator may cancel active tasks."
    )]
    pub async fn cancel_task(
        &self,
        Parameters(req): Parameters<ControlTaskRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let resp: TaskResponse = self
            .daemon
            .request(
                MessageType::CancelTask,
                &CancelTaskPayload {
                    id: req.id,
                    reason: req.reason.unwrap_or_default(),
                    expected_version: req.expected_version,
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.task).unwrap_or_default(),
        )]))
    }

    /// `remove_task` — soft-delete / archive a terminal task.
    #[tool(
        description = "Soft-delete/archive a terminal task so it no longer appears in list results by default."
    )]
    pub async fn remove_task(
        &self,
        Parameters(req): Parameters<ControlTaskRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let resp: TaskResponse = self
            .daemon
            .request(
                MessageType::RemoveTask,
                &RemoveTaskPayload {
                    id: req.id,
                    reason: req.reason.unwrap_or_default(),
                    expected_version: req.expected_version,
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.task).unwrap_or_default(),
        )]))
    }

    /// `intervene_task` — bounded recovery action (`wake`,
    /// `interrupt`, or `retry`) for a pending/in_progress/blocked
    /// task.
    #[tool(
        description = "Apply a bounded, task-targeted recovery action for a stuck pending/in_progress task."
    )]
    pub async fn intervene_task(
        &self,
        Parameters(req): Parameters<InterveneTaskRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let resp: InterveneTaskResponse = self
            .daemon
            .request(
                MessageType::InterveneTask,
                &InterveneTaskPayload {
                    id: req.id,
                    action: req.action,
                    note: req.note.unwrap_or_default(),
                    expected_version: req.expected_version,
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_default(),
        )]))
    }

    /// `list_agents` — configured agents from the active ax config,
    /// enriched with active-workspace status from the daemon.
    #[tool(
        description = "List configured ax agents from the active ax config, enriched with current active status when available. Supports filtering to help find a specific agent such as a portfolio development team agent."
    )]
    pub async fn list_agents(
        &self,
        Parameters(req): Parameters<ListAgentsRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let cfg_path = self
            .resolve_tool_config_path()
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let cfg = Config::load(&cfg_path)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("load ax config: {e}"), None))?;
        let active: ListWorkspacesResponse = self
            .daemon
            .request(MessageType::ListWorkspaces, &serde_json::json!({}))
            .await
            .map_err(tool_error)?;

        let mut active_by_name: BTreeMap<String, WorkspaceInfo> = BTreeMap::new();
        for ws in &active.workspaces {
            active_by_name.insert(ws.name.clone(), ws.clone());
        }
        let query = req
            .query
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_default();

        let mut agents: Vec<serde_json::Value> = Vec::new();
        for (name, ws) in &cfg.workspaces {
            let mut info = serde_json::Map::new();
            info.insert("name".into(), serde_json::Value::String(name.clone()));
            info.insert("dir".into(), serde_json::Value::String(ws.dir.clone()));
            if !ws.description.is_empty() {
                info.insert(
                    "description".into(),
                    serde_json::Value::String(ws.description.clone()),
                );
            }

            let launch_mode;
            let mut runtime_name: Option<String> = None;
            let mut command: Option<String> = None;
            let mut instruction_file: Option<String> = None;
            if ws.agent == "none" {
                launch_mode = "manual";
            } else if !ws.agent.trim().is_empty() {
                launch_mode = "custom";
                command = Some(ws.agent.clone());
            } else {
                launch_mode = "runtime";
                let rt = ax_agent::Runtime::normalize(&ws.runtime)
                    .map_or_else(|| ws.runtime.clone(), |r| r.as_str().to_owned());
                instruction_file = ax_agent::instruction_file(&ws.runtime).map(str::to_owned);
                runtime_name = Some(rt);
            }
            if let Some(runtime) = &runtime_name {
                info.insert("runtime".into(), serde_json::Value::String(runtime.clone()));
            }
            info.insert(
                "launch_mode".into(),
                serde_json::Value::String(launch_mode.into()),
            );
            if let Some(cmd) = &command {
                info.insert("command".into(), serde_json::Value::String(cmd.clone()));
            }

            let active_entry = active_by_name.get(name).cloned();
            let is_active = active_entry.is_some();
            info.insert("active".into(), serde_json::Value::Bool(is_active));
            let state = if is_active {
                if ax_tmux::is_idle(name) {
                    "idle"
                } else {
                    "running"
                }
            } else {
                "offline"
            };
            info.insert("state".into(), serde_json::Value::String(state.into()));
            if let Some(ws_info) = active_entry {
                let status_str = match ws_info.status {
                    AgentStatus::Online => "online",
                    AgentStatus::Offline => "offline",
                    AgentStatus::Disconnected => "disconnected",
                };
                info.insert(
                    "status".into(),
                    serde_json::Value::String(status_str.into()),
                );
                if !ws_info.status_text.is_empty() {
                    info.insert(
                        "status_text".into(),
                        serde_json::Value::String(ws_info.status_text),
                    );
                }
                if let Some(ts) = ws_info.connected_at {
                    info.insert(
                        "connected_at".into(),
                        serde_json::Value::String(ts.to_rfc3339()),
                    );
                }
            }
            if let Some(path) = instruction_file {
                info.insert("instruction_file".into(), serde_json::Value::String(path));
            }
            let preview = instruction_preview(&ws.instructions);
            if !preview.is_empty() {
                info.insert(
                    "instructions_preview".into(),
                    serde_json::Value::String(preview),
                );
            }

            if req.active_only && !is_active {
                continue;
            }
            if !query.is_empty() && !matches_agent_query(&info, &query) {
                continue;
            }
            agents.push(serde_json::Value::Object(info));
        }
        agents.sort_by(|a, b| {
            a.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or_default())
        });

        let body = serde_json::json!({
            "project": cfg.project,
            "config_path": cfg_path.display().to_string(),
            "agent_count": agents.len(),
            "active_count": active.workspaces.len(),
            "agents": agents,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )]))
    }

    /// `start_agent` — bring the workspace's managed session up.
    #[tool(
        description = "Start a configured workspace agent or managed child orchestrator by exact name. Root orchestrator lifecycle is not supported by this MCP surface."
    )]
    pub async fn start_agent(
        &self,
        Parameters(req): Parameters<AgentNameRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.agent_lifecycle(req.name, LifecycleAction::Start).await
    }

    /// `stop_agent` — tear down the workspace's managed session.
    #[tool(
        description = "Stop a configured workspace agent or managed child orchestrator by exact name. This removes the managed session and cleans generated artifacts for that target."
    )]
    pub async fn stop_agent(
        &self,
        Parameters(req): Parameters<AgentNameRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.agent_lifecycle(req.name, LifecycleAction::Stop).await
    }

    /// `restart_agent` — recycle the workspace's managed session from
    /// scratch.
    #[tool(
        description = "Restart a configured workspace agent or managed child orchestrator by exact name from a fresh managed session. Root orchestrator lifecycle is not supported by this MCP surface."
    )]
    pub async fn restart_agent(
        &self,
        Parameters(req): Parameters<AgentNameRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.agent_lifecycle(req.name, LifecycleAction::Restart)
            .await
    }

    /// `interrupt_agent` — Escape the target workspace's tmux pane
    /// without killing the session.
    #[tool(
        description = "Send Escape to a workspace tmux session to interrupt the agent's current interactive CLI action without killing the session."
    )]
    pub async fn interrupt_agent(
        &self,
        Parameters(req): Parameters<InterruptAgentRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let name = req.name.trim();
        if name.is_empty() {
            return Err(rmcp::ErrorData::invalid_params("name is required", None));
        }
        if !ax_tmux::session_exists(name) {
            return Err(rmcp::ErrorData::internal_error(
                format!("Workspace {name:?} is not running"),
                None,
            ));
        }
        ax_tmux::interrupt_workspace(name).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Failed to interrupt {name:?}: {e}"), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Interrupt sent to {name:?}"
        ))]))
    }

    /// `send_keys` — forward a sequence of literal or named tmux keys
    /// to the workspace pane. Used to clear blocking interactive
    /// prompts inside an agent CLI.
    #[tool(
        description = "Send a sequence of keystrokes to a workspace's tmux session. Use this to resolve blocking interactive dialogs in an agent CLI (e.g. Claude Code's \"Resuming from summary\" 1/2/3 prompt). Each element is either a named special key (Enter, Escape, Tab, Space, BSpace, Up/Down/Left/Right, Home/End, PageUp/PageDown, Ctrl-C/Ctrl-D/Ctrl-U/...) or literal text that will be typed verbatim. Example: keys=[\"2\",\"Enter\"] selects the second option and submits it."
    )]
    pub async fn send_keys(
        &self,
        Parameters(req): Parameters<SendKeysRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let workspace = req.workspace.trim();
        if workspace.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "workspace is required",
                None,
            ));
        }
        if req.keys.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "keys must contain at least one entry",
                None,
            ));
        }
        if !ax_tmux::session_exists(workspace) {
            return Err(rmcp::ErrorData::internal_error(
                format!("Workspace {workspace:?} is not running"),
                None,
            ));
        }
        let refs: Vec<&str> = req.keys.iter().map(String::as_str).collect();
        ax_tmux::send_keys(workspace, &refs).map_err(|e| {
            rmcp::ErrorData::internal_error(
                format!("Failed to send keys to {workspace:?}: {e}"),
                None,
            )
        })?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Sent {} key(s) to {workspace:?}: {}",
            req.keys.len(),
            req.keys.join(" ")
        ))]))
    }

    /// `get_team_state` — read the daemon-managed effective team
    /// reconfiguration state.
    #[tool(
        description = "Read the daemon-managed effective team state for experimental MCP team reconfiguration."
    )]
    pub async fn get_team_state(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let cfg_path = self
            .resolve_base_config_path()
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let resp: TeamStateResponse = self
            .daemon
            .request(
                MessageType::GetTeamState,
                &GetTeamStatePayload {
                    config_path: cfg_path.display().to_string(),
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.state).unwrap_or_default(),
        )]))
    }

    /// `plan_initial_team` — read-only project survey for teams
    /// that haven't been initialised yet. Returns an axis suggestion,
    /// the top-level directory layout, and a README excerpt so a
    /// calling agent can compose a workspace plan without scanning
    /// the filesystem itself. Does not write anything.
    #[tool(
        description = "Survey a project directory and return the signals (axis suggestion, top-level dirs, README excerpt) an agent needs to propose an initial ax team. Read-only; does not write any config."
    )]
    pub async fn plan_initial_team(
        &self,
        Parameters(req): Parameters<PlanInitialTeamRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_dir = resolve_project_dir(req.project_dir.as_deref(), None)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        let plan = crate::planner::plan_initial_team(&project_dir).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("plan_initial_team: {e}"), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&plan).unwrap_or_default(),
        )]))
    }

    /// `plan_team_reconfigure` — compare the current ax config
    /// against the project on disk and surface drift (orphan
    /// directories, empty workspaces, axis headers). Complements
    /// `apply_team_reconfigure`: call this first to see what
    /// actually changed, then hand the curated changes to apply.
    #[tool(
        description = "Compare the existing ax config at the server's effective config path against the project on disk. Returns current axis, per-workspace exists/non-empty flags, orphan top-level dirs, and empty workspaces. Read-only; does not write or reconcile."
    )]
    pub async fn plan_team_reconfigure(
        &self,
        Parameters(req): Parameters<PlanTeamReconfigureRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let cfg_path = self
            .resolve_base_config_path()
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let project_dir = resolve_project_dir(req.project_dir.as_deref(), Some(&cfg_path))
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        let plan = crate::planner::plan_team_reconfigure(&project_dir, &cfg_path).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("plan_team_reconfigure: {e}"), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&plan).unwrap_or_default(),
        )]))
    }

    /// `dry_run_team_reconfigure` — validate a planned overlay diff
    /// without mutating the runtime.
    #[tool(
        description = "Plan supported v1 team changes against the daemon-managed effective state without reconciling runtime."
    )]
    pub async fn dry_run_team_reconfigure(
        &self,
        Parameters(req): Parameters<TeamReconfigureRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if req.changes.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "changes must contain at least one entry",
                None,
            ));
        }
        let cfg_path = self
            .resolve_base_config_path()
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let changes =
            convert_changes(req.changes).map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        let payload = TeamReconfigurePayload {
            config_path: cfg_path.display().to_string(),
            expected_revision: req.expected_revision,
            changes,
            reconcile_mode: None,
        };
        let resp: TeamPlanResponse = self
            .daemon
            .request(MessageType::DryRunTeam, &payload)
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp.plan).unwrap_or_default(),
        )]))
    }

    /// `apply_team_reconfigure` — commit a team overlay change and
    /// run the requested reconcile against the runtime state.
    #[tool(
        description = "Apply supported v1 team changes via the daemon-managed effective state and run the requested reconcile mode."
    )]
    pub async fn apply_team_reconfigure(
        &self,
        Parameters(req): Parameters<TeamReconfigureRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if req.changes.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "changes must contain at least one entry",
                None,
            ));
        }
        let reconcile_mode = parse_reconcile_mode(req.reconcile_mode.as_deref())
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?
            .unwrap_or(TeamReconcileMode::ArtifactsOnly);
        let cfg_path = self
            .resolve_base_config_path()
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let changes =
            convert_changes(req.changes).map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        let apply_payload = TeamReconfigurePayload {
            config_path: cfg_path.display().to_string(),
            expected_revision: req.expected_revision,
            changes,
            reconcile_mode: Some(reconcile_mode.clone()),
        };
        let apply: TeamApplyResponse = self
            .daemon
            .request(MessageType::ApplyTeam, &apply_payload)
            .await
            .map_err(tool_error)?;
        let ticket = apply.ticket;
        let socket_path = self.daemon.socket_path().to_path_buf();

        let reconcile = reconcile_applied_team(&ticket, &socket_path);
        let (report, reconcile_err) = match reconcile {
            Ok(report) => (report, None),
            Err(err) => (ReconcileReport::default(), Some(err)),
        };
        let actions = team_actions_from_reconcile_report(&report);

        if let Some(err) = reconcile_err {
            let finish = FinishTeamReconfigurePayload {
                token: ticket.token.clone(),
                success: false,
                error: err.clone(),
                actions,
            };
            let finalize = self
                .daemon
                .request::<_, TeamStateResponse>(MessageType::FinishTeam, &finish)
                .await;
            if let Err(finish_err) = finalize {
                return Err(rmcp::ErrorData::internal_error(
                    format!(
                        "Team reconfiguration {:?} failed during reconcile: {err} (finalize error: {finish_err})",
                        ticket.token
                    ),
                    None,
                ));
            }
            return Err(rmcp::ErrorData::internal_error(
                format!(
                    "Team reconfiguration {:?} failed during reconcile: {err}",
                    ticket.token
                ),
                None,
            ));
        }

        let finish = FinishTeamReconfigurePayload {
            token: ticket.token.clone(),
            success: true,
            error: String::new(),
            actions,
        };
        let state_resp: TeamStateResponse = self
            .daemon
            .request(MessageType::FinishTeam, &finish)
            .await
            .map_err(|e| {
                rmcp::ErrorData::internal_error(
                    format!(
                        "Failed to finalize team reconfiguration {:?}: {e}",
                        ticket.token
                    ),
                    None,
                )
            })?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "ticket": ticket,
                "state": state_resp.state,
                "reconcile": report,
            }))
            .unwrap_or_default(),
        )]))
    }

    /// `broadcast_message` — fan out a single message to every other
    /// registered workspace.
    #[tool(description = "Send a message to all other workspace agents.")]
    pub async fn broadcast_message(
        &self,
        Parameters(req): Parameters<BroadcastRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let dispatch_path = self
            .effective_config()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let resp: BroadcastResponse = self
            .daemon
            .request(
                MessageType::Broadcast,
                &BroadcastPayload {
                    message: req.message.clone(),
                    config_path: dispatch_path,
                },
            )
            .await
            .map_err(tool_error)?;
        if resp.recipients.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No other workspaces to broadcast to.",
            )]));
        }
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Broadcast sent to {} workspace(s): {}",
            resp.recipients.len(),
            resp.recipients.join(", ")
        ))]))
    }

    /// `inspect_agent` — ask a target workspace for a status summary
    /// and block until it replies (or the timeout expires).
    #[tool(
        description = "Ask a specific ax agent to summarize its current operating state. Useful after list_agents finds a target such as a portfolio development team agent."
    )]
    pub async fn inspect_agent(
        &self,
        Parameters(req): Parameters<InspectAgentRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let cfg_path = self
            .resolve_tool_config_path()
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let cfg = Config::load(&cfg_path)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("load ax config: {e}"), None))?;
        let ws = cfg.workspaces.get(&req.name).ok_or_else(|| {
            rmcp::ErrorData::invalid_params(
                format!(
                    "Agent {:?} is not defined in {}",
                    req.name,
                    cfg_path.display()
                ),
                None,
            )
        })?;

        let question = req
            .question
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| DEFAULT_INSPECT_QUESTION.to_owned(), ToOwned::to_owned);
        let timeout = req.timeout.unwrap_or(120).max(1);

        let caller = self.daemon.workspace().to_owned();
        let full_message = format!(
            "{question}\n\n[ax] 작업 완료 후 반드시 send_message(to=\"{caller}\") 로 결과를 보내주세요."
        );

        let sent = match self
            .send_workspace_message(&req.name, &full_message, &cfg_path)
            .await
        {
            Ok(sent) => sent,
            Err(e) => return Ok(tool_execution_error(format!("Failed to send message: {e}"))),
        };
        if sent.message_id.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Inspection request to {:?} was suppressed as a duplicate no-op/status update.",
                req.name
            ))]));
        }

        let reply = match self.poll_for_reply(&req.name, timeout).await {
            Ok(reply) => reply,
            Err(e) => return Ok(tool_execution_error(e)),
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "agent": req.name,
                "description": ws.description,
                "question": question,
                "status_reply": reply,
            }))
            .unwrap_or_default(),
        )]))
    }

    /// `request` — synchronous message to another workspace with a
    /// reply-polling deadline. Recipients are told to call
    /// `send_message` back once they are done.
    #[tool(
        description = "Send a task to another workspace agent and wait for the reply. This wakes the target agent via tmux and polls for a response. Use this instead of send_message when you need the result back."
    )]
    pub async fn request_message(
        &self,
        Parameters(req): Parameters<RequestMessageRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let cfg_path = self
            .resolve_tool_config_path()
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let timeout = req.timeout.unwrap_or(120).max(1);
        let caller = self.daemon.workspace().to_owned();
        let full_message = format!(
            "{}\n\n[ax/request] 이 메시지는 동기 요청입니다. `{caller}`가 당신의 응답을 기다리고 있습니다. 작업이 끝나면 즉시 `send_message(to=\"{caller}\")`로 결과를 회신하세요. 하위 워크스페이스에 위임할 때는 `request`가 아닌 `send_message`를 병렬로 사용한 뒤 `read_messages`로 수집하세요.",
            req.message
        );

        let sent = match self
            .send_workspace_message(&req.to, &full_message, &cfg_path)
            .await
        {
            Ok(sent) => sent,
            Err(e) => return Ok(tool_execution_error(format!("Failed to send message: {e}"))),
        };
        if sent.message_id.is_empty() {
            return Ok(tool_execution_error(format!(
                "Request to {:?} was suppressed as a duplicate no-op/status update",
                req.to
            )));
        }

        let reply = match self.poll_for_reply(&req.to, timeout).await {
            Ok(reply) => reply,
            Err(e) => return Ok(tool_execution_error(e)),
        };
        Ok(CallToolResult::success(vec![Content::text(reply)]))
    }

    /// `get_usage_trends` — recent Claude/Codex token trends per
    /// workspace, optionally filtered by name.
    #[tool(
        description = "Return recent Claude token trend data for active ax workspaces, including per-agent breakout when transcript attribution is available."
    )]
    pub async fn get_usage_trends(
        &self,
        Parameters(req): Parameters<UsageTrendsRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let workspace = req
            .workspace
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_owned();
        let since_minutes = req.since_minutes.filter(|v| *v > 0).unwrap_or(180);
        let bucket_minutes = req.bucket_minutes.filter(|v| *v > 0).unwrap_or(5);

        let requests = self
            .build_usage_trend_requests(&workspace)
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

        let resp: UsageTrendsResponse = self
            .daemon
            .request(
                MessageType::UsageTrends,
                &UsageTrendsPayload {
                    workspaces: requests,
                    since_minutes,
                    bucket_minutes,
                },
            )
            .await
            .map_err(tool_error)?;

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "workspace": workspace,
                "since_minutes": since_minutes,
                "bucket_minutes": bucket_minutes,
                "trends": resp.trends,
            }))
            .unwrap_or_default(),
        )]))
    }
}

impl Server {
    async fn agent_lifecycle(
        &self,
        name: String,
        action: LifecycleAction,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let cfg_path = self
            .resolve_tool_config_path()
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let resp: AgentLifecycleResponse = self
            .daemon
            .request(
                MessageType::AgentLifecycle,
                &AgentLifecyclePayload {
                    config_path: cfg_path.display().to_string(),
                    name,
                    action,
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_default(),
        )]))
    }

    /// Resolve the config path the tool handlers should use. If the
    /// daemon reports an experimental team-reconfigure overlay is
    /// active, prefer the overlay's effective config; otherwise fall
    /// back to the base path recorded on the server.
    async fn resolve_tool_config_path(&self) -> Result<PathBuf, String> {
        let base = self.resolve_base_config_path()?;
        let state: TeamStateResponse = match self
            .daemon
            .request(
                MessageType::GetTeamState,
                &GetTeamStatePayload {
                    config_path: base.display().to_string(),
                },
            )
            .await
        {
            Ok(resp) => resp,
            Err(_) => return Ok(base),
        };
        if state.state.feature_enabled && !state.state.effective_config_path.trim().is_empty() {
            return Ok(PathBuf::from(state.state.effective_config_path));
        }
        Ok(base)
    }

    async fn send_workspace_message(
        &self,
        target: &str,
        message: &str,
        config_path: &Path,
    ) -> Result<SendMessageResponse, DaemonClientError> {
        self.daemon
            .request::<_, SendMessageResponse>(
                MessageType::SendMessage,
                &SendMessagePayload {
                    to: target.to_owned(),
                    message: message.to_owned(),
                    config_path: config_path.display().to_string(),
                },
            )
            .await
    }

    async fn poll_for_reply(&self, from: &str, timeout_secs: i64) -> Result<String, String> {
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1) as u64);
        loop {
            if Instant::now() >= deadline {
                return Err(format!(
                    "Timeout: no reply from {from:?} within {timeout_secs}s"
                ));
            }
            let resp: ReadMessagesResponse = self
                .daemon
                .request(
                    MessageType::ReadMessages,
                    &ReadMessagesPayload {
                        limit: 10,
                        from: from.to_owned(),
                    },
                )
                .await
                .map_err(|e| format!("read_messages: {e}"))?;
            if !resp.messages.is_empty() {
                let mut out = String::new();
                for msg in &resp.messages {
                    out.push_str(&msg.content);
                    out.push('\n');
                }
                return Ok(out.trim().to_owned());
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    async fn build_usage_trend_requests(
        &self,
        workspace_name: &str,
    ) -> Result<Vec<UsageTrendWorkspace>, String> {
        let active: ListWorkspacesResponse = self
            .daemon
            .request(MessageType::ListWorkspaces, &serde_json::json!({}))
            .await
            .map_err(|e| format!("list active workspaces: {e}"))?;
        let mut active_by_name: BTreeMap<String, WorkspaceInfo> = BTreeMap::new();
        for ws in &active.workspaces {
            active_by_name.insert(ws.name.clone(), ws.clone());
        }

        if !workspace_name.is_empty() {
            if let Some(ws) = active_by_name.get(workspace_name) {
                if !ws.dir.trim().is_empty() {
                    return Ok(vec![UsageTrendWorkspace {
                        workspace: workspace_name.to_owned(),
                        cwd: ws.dir.trim().to_owned(),
                    }]);
                }
            }
            let cfg_path = self.resolve_tool_config_path().await?;
            let cfg = Config::load(&cfg_path)
                .map_err(|e| format!("load ax config for workspace {workspace_name:?}: {e}"))?;
            if let Some(ws) = cfg.workspaces.get(workspace_name) {
                if !ws.dir.trim().is_empty() {
                    return Ok(vec![UsageTrendWorkspace {
                        workspace: workspace_name.to_owned(),
                        cwd: ws.dir.trim().to_owned(),
                    }]);
                }
            }
            return Err(format!(
                "workspace {workspace_name:?} not found in active registry or {}",
                cfg_path.display()
            ));
        }

        let mut requests: Vec<UsageTrendWorkspace> = Vec::with_capacity(active.workspaces.len());
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for ws in &active.workspaces {
            if !seen.insert(ws.name.clone()) {
                continue;
            }
            let cwd = ws.dir.trim();
            if cwd.is_empty() {
                continue;
            }
            requests.push(UsageTrendWorkspace {
                workspace: ws.name.clone(),
                cwd: cwd.to_owned(),
            });
        }
        requests.sort_by(|a, b| a.workspace.cmp(&b.workspace));
        Ok(requests)
    }

    fn resolve_base_config_path(&self) -> Result<PathBuf, String> {
        if let Some(path) = self.config_path.clone() {
            return Ok(path);
        }
        crate::memory_scope::find_effective_config(None)
            .ok_or_else(|| "ax config file not found".to_owned())
    }

    async fn memory_query(
        &self,
        req: MemoryQueryRequest,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let scopes = memory_scope::resolve_many(
            &req.scopes,
            self.daemon.workspace(),
            self.effective_config().as_deref(),
        )
        .map_err(|e| rmcp::ErrorData::invalid_params(e.to_string(), None))?;
        let resp: RecallMemoriesResponse = self
            .daemon
            .request(
                MessageType::RecallMemories,
                &RecallMemoriesPayload {
                    scopes: scopes.clone(),
                    kind: req.kind.unwrap_or_default(),
                    tags: req.tags,
                    include_superseded: req.include_superseded,
                    limit: req.limit,
                },
            )
            .await
            .map_err(tool_error)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "scopes": scopes,
                "count": resp.memories.len(),
                "memories": resp.memories,
            }))
            .unwrap_or_default(),
        )]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        let server_info = Implementation::new("ax", env!("CARGO_PKG_VERSION"));
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_server_info(server_info)
            .with_instructions(self.instructions())
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let tool_name = request.name.to_string();
        let started = Instant::now();
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await;
        let duration_ms = started.elapsed().as_millis() as u64;
        let (ok, telemetry_err_kind, activity_err_kind) = match &result {
            Ok(tool_result) if tool_result.is_error == Some(true) => (
                false,
                call_tool_result_error_text(tool_result),
                "TOOL_ERROR".to_owned(),
            ),
            Ok(_) => (true, String::new(), String::new()),
            Err(e) => (false, e.message.to_string(), mcp_error_kind(e)),
        };
        self.record_tool_call(&tool_name, ok, duration_ms, &telemetry_err_kind);
        self.record_mcp_tool_activity(&tool_name, ok, duration_ms, &activity_err_kind);
        result
    }
}

#[allow(clippy::needless_pass_by_value)]
fn tool_error(err: DaemonClientError) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(err.to_string(), None)
}

fn tool_execution_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(message.into())])
}

fn daemon_tool_execution_error(err: DaemonClientError) -> CallToolResult {
    tool_execution_error(err.to_string())
}

fn call_tool_result_error_text(result: &CallToolResult) -> String {
    let text = result
        .content
        .iter()
        .filter_map(|content| content.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        "tool returned isError=true".to_owned()
    } else {
        text
    }
}

fn mcp_error_kind(err: &rmcp::ErrorData) -> String {
    match err.code {
        rmcp::model::ErrorCode::RESOURCE_NOT_FOUND => "RESOURCE_NOT_FOUND".to_owned(),
        rmcp::model::ErrorCode::INVALID_REQUEST => "INVALID_REQUEST".to_owned(),
        rmcp::model::ErrorCode::METHOD_NOT_FOUND => "METHOD_NOT_FOUND".to_owned(),
        rmcp::model::ErrorCode::INVALID_PARAMS => "INVALID_PARAMS".to_owned(),
        rmcp::model::ErrorCode::INTERNAL_ERROR => "INTERNAL_ERROR".to_owned(),
        rmcp::model::ErrorCode::PARSE_ERROR => "PARSE_ERROR".to_owned(),
        rmcp::model::ErrorCode::URL_ELICITATION_REQUIRED => "URL_ELICITATION_REQUIRED".to_owned(),
        rmcp::model::ErrorCode(code) => format!("ERROR_CODE_{code}"),
    }
}

fn sanitize_mcp_activity_field(raw: &str) -> String {
    let compact = raw.trim().replace(['\n', '\r'], " ");
    if compact.chars().count() <= MCP_ACTIVITY_FIELD_LIMIT {
        return compact;
    }
    let mut out: String = compact.chars().take(MCP_ACTIVITY_FIELD_LIMIT).collect();
    out.push_str("...");
    out
}

/// Resolve the project root that a planner tool should scan.
///
/// 1. Explicit `project_dir` arg wins; must be absolute.
/// 2. Otherwise, the parent of `config_hint` (expected `<root>/.ax/config.yaml`).
/// 3. Otherwise, the MCP server's current working directory.
fn resolve_project_dir(
    project_dir: Option<&str>,
    config_hint: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(raw) = project_dir {
        let p = PathBuf::from(raw);
        if !p.is_absolute() {
            return Err(format!("project_dir must be absolute, got {raw:?}"));
        }
        return Ok(p);
    }
    if let Some(cfg) = config_hint {
        // Walk up from `.ax/config.yaml` twice: once to `.ax/`, once to the project root.
        if let Some(parent) = cfg.parent().and_then(Path::parent) {
            return Ok(parent.to_path_buf());
        }
    }
    std::env::current_dir().map_err(|e| format!("resolve current_dir: {e}"))
}

fn validate_lifecycle_options(
    start_mode: Option<&str>,
    workflow_mode: Option<&str>,
    priority: Option<&str>,
    stale_after_seconds: Option<i64>,
) -> Result<(), rmcp::ErrorData> {
    if let Some(s) = stale_after_seconds {
        if s < 0 {
            return Err(rmcp::ErrorData::invalid_params(
                "stale_after_seconds must be >= 0",
                None,
            ));
        }
    }
    if let Some(v) = start_mode.map(str::trim) {
        if !matches!(v, "" | "default" | "fresh") {
            return Err(rmcp::ErrorData::invalid_params(
                format!("invalid start_mode {v:?} (must be default or fresh)"),
                None,
            ));
        }
    }
    if let Some(v) = workflow_mode.map(str::trim) {
        if !matches!(v, "" | "parallel" | "serial") {
            return Err(rmcp::ErrorData::invalid_params(
                format!("invalid workflow_mode {v:?} (must be parallel or serial)"),
                None,
            ));
        }
    }
    if let Some(v) = priority.map(str::trim) {
        if !matches!(v, "" | "low" | "normal" | "high" | "urgent") {
            return Err(rmcp::ErrorData::invalid_params(
                format!("invalid priority {v:?} (must be low, normal, high, or urgent)"),
                None,
            ));
        }
    }
    Ok(())
}

fn parse_update_status(raw: &str) -> Result<TaskStatus, String> {
    match raw {
        "pending" => Ok(TaskStatus::Pending),
        "in_progress" => Ok(TaskStatus::InProgress),
        "completed" => Ok(TaskStatus::Completed),
        "failed" => Ok(TaskStatus::Failed),
        other => Err(format!(
            "invalid status {other:?} (must be pending, in_progress, completed, or failed)"
        )),
    }
}

fn parse_list_status(raw: Option<&str>) -> Result<Option<TaskStatus>, String> {
    let Some(value) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    match value {
        "pending" => Ok(Some(TaskStatus::Pending)),
        "in_progress" => Ok(Some(TaskStatus::InProgress)),
        "completed" => Ok(Some(TaskStatus::Completed)),
        "failed" => Ok(Some(TaskStatus::Failed)),
        "cancelled" => Ok(Some(TaskStatus::Cancelled)),
        other => Err(format!("invalid status filter {other:?}")),
    }
}

fn status_rename(status: &TaskStatus) -> String {
    match status {
        TaskStatus::Pending => "pending".into(),
        TaskStatus::InProgress => "in_progress".into(),
        TaskStatus::Blocked => "blocked".into(),
        TaskStatus::Completed => "completed".into(),
        TaskStatus::Failed => "failed".into(),
        TaskStatus::Cancelled => "cancelled".into(),
    }
}

#[derive(Debug, Clone, Copy)]
enum WorkspaceView {
    Assigned,
    Created,
    Both,
}

impl WorkspaceView {
    fn as_str(self) -> &'static str {
        match self {
            Self::Assigned => "assigned",
            Self::Created => "created",
            Self::Both => "both",
        }
    }
}

fn instruction_preview(instructions: &str) -> String {
    let trimmed = instructions.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut parts = trimmed.split_whitespace();
    let mut out: Vec<&str> = Vec::with_capacity(24);
    for _ in 0..24 {
        match parts.next() {
            Some(word) => out.push(word),
            None => break,
        }
    }
    out.join(" ")
}

fn matches_agent_query(info: &serde_json::Map<String, serde_json::Value>, query: &str) -> bool {
    const FIELDS: &[&str] = &[
        "name",
        "dir",
        "description",
        "runtime",
        "command",
        "state",
        "instructions_preview",
        "status_text",
    ];
    for field in FIELDS {
        if let Some(text) = info.get(*field).and_then(|v| v.as_str()) {
            if text.to_ascii_lowercase().contains(query) {
                return true;
            }
        }
    }
    false
}

fn reconcile_applied_team(
    ticket: &TeamApplyTicket,
    socket_path: &Path,
) -> Result<ReconcileReport, String> {
    let effective = ticket.plan.state.effective_config_path.trim();
    let base = ticket.plan.state.base_config_path.trim();
    let effective_path = if !effective.is_empty() {
        effective
    } else if !base.is_empty() {
        base
    } else {
        return Err("team apply ticket is missing an effective config path".into());
    };
    let effective_path = PathBuf::from(effective_path);

    let cfg = Config::load(&effective_path).map_err(|e| format!("load effective config: {e}"))?;
    let tree = Config::load_tree(&effective_path)
        .map_err(|e| format!("load effective config tree: {e}"))?;
    let include_root = !tree_disables_root(&tree);
    let desired = build_desired_state_with_tree(
        &cfg,
        &tree,
        socket_path.to_path_buf(),
        effective_path.clone(),
        include_root,
    )
    .map_err(|e| format!("build desired runtime state: {e}"))?;

    let reconciler = Reconciler::new(socket_path.to_path_buf(), effective_path, ax_bin_path());
    reconciler
        .reconcile_desired_state(
            &desired,
            ReconcileOptions {
                daemon_running: ticket.reconcile_mode == TeamReconcileMode::StartMissing,
                allow_disruptive_changes: ticket.reconcile_mode == TeamReconcileMode::StartMissing,
            },
        )
        .map_err(|e| e.to_string())
}

fn tree_disables_root(tree: &ProjectNode) -> bool {
    tree.disable_root_orchestrator
}

fn ax_bin_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ax"))
}

fn team_actions_from_reconcile_report(report: &ReconcileReport) -> Vec<TeamReconfigureAction> {
    use ax_proto::types::TeamEntryKind;

    let mut actions: Vec<TeamReconfigureAction> = Vec::with_capacity(report.actions.len() + 1);
    for action in &report.actions {
        let Some(kind) = reconcile_action_kind(&action.kind) else {
            continue;
        };
        actions.push(TeamReconfigureAction {
            action: action.operation.clone(),
            kind,
            name: action.name.clone(),
            dir: String::new(),
            detail: action.details.clone(),
        });
    }
    if report.root_manual_restart_required {
        actions.push(TeamReconfigureAction {
            action: "manual_restart_required".into(),
            kind: TeamEntryKind::RootOrchestrator,
            name: "orchestrator".into(),
            dir: String::new(),
            detail: report.root_manual_restart_reasons.join("; "),
        });
    }
    actions
}

fn reconcile_action_kind(value: &str) -> Option<TeamEntryKind> {
    match value.trim() {
        "workspace" => Some(TeamEntryKind::Workspace),
        "orchestrator" => Some(TeamEntryKind::RootOrchestrator),
        _ => None,
    }
}

fn parse_reconcile_mode(raw: Option<&str>) -> Result<Option<TeamReconcileMode>, String> {
    match raw.map(str::trim).unwrap_or_default() {
        "" => Ok(None),
        "artifacts_only" => Ok(Some(TeamReconcileMode::ArtifactsOnly)),
        "start_missing" => Ok(Some(TeamReconcileMode::StartMissing)),
        other => Err(format!(
            "invalid reconcile_mode {other:?} (must be artifacts_only or start_missing)"
        )),
    }
}

fn convert_changes(
    changes: Vec<TeamReconfigureChangeInput>,
) -> Result<Vec<TeamReconfigureChange>, String> {
    changes.into_iter().map(convert_change).collect()
}

fn convert_change(input: TeamReconfigureChangeInput) -> Result<TeamReconfigureChange, String> {
    let op = match input.op.trim() {
        "add" => TeamChangeOp::Add,
        "remove" => TeamChangeOp::Remove,
        "enable" => TeamChangeOp::Enable,
        "disable" => TeamChangeOp::Disable,
        other => return Err(format!("invalid change op {other:?}")),
    };
    let kind = match input.kind.trim() {
        "workspace" => TeamEntryKind::Workspace,
        "child" => TeamEntryKind::Child,
        "root_orchestrator" => TeamEntryKind::RootOrchestrator,
        other => return Err(format!("invalid change kind {other:?}")),
    };
    Ok(TeamReconfigureChange {
        op,
        kind,
        name: input.name.unwrap_or_default(),
        workspace: input.workspace.map(TeamWorkspaceSpecInput::into_spec),
        child: input.child.map(TeamChildSpecInput::into_spec),
    })
}

impl TeamWorkspaceSpecInput {
    fn into_spec(self) -> TeamWorkspaceSpec {
        TeamWorkspaceSpec {
            dir: self.dir,
            description: self.description.unwrap_or_default(),
            shell: self.shell.unwrap_or_default(),
            runtime: self.runtime.unwrap_or_default(),
            codex_model_reasoning_effort: self.codex_model_reasoning_effort.unwrap_or_default(),
            agent: self.agent.unwrap_or_default(),
            instructions: self.instructions.unwrap_or_default(),
            env: self.env.unwrap_or_default(),
        }
    }
}

impl TeamChildSpecInput {
    fn into_spec(self) -> TeamChildSpec {
        TeamChildSpec {
            dir: self.dir,
            prefix: self.prefix.unwrap_or_default(),
        }
    }
}

fn parse_workspace_view(raw: Option<&str>) -> Result<WorkspaceView, String> {
    match raw.map(str::trim).unwrap_or_default() {
        "" | "both" => Ok(WorkspaceView::Both),
        "assigned" => Ok(WorkspaceView::Assigned),
        "created" => Ok(WorkspaceView::Created),
        other => Err(format!(
            "invalid view {other:?} (must be assigned, created, or both)"
        )),
    }
}
