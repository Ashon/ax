//! MCP server scaffold + tool registrations that delegate to the
//! daemon client. Mirrors `internal/mcpserver/server.go` + the
//! `tools_shared.go` / `tools_workspace.go` / `tools_memory.go` /
//! `tools_messages.go` groups. Remaining groups (usage, tasks,
//! agents, `team_reconfigure`) land in follow-up commits.
//!
//! Each tool body calls into the `DaemonClient` using the typed
//! envelope payloads and returns JSON-formatted text through
//! `CallToolResult::success`, keeping the output byte-compatible with
//! what Go's `mcp.NewToolResultText(json.MarshalIndent(...))` emits.

use std::path::{Path, PathBuf};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::service::RunningService;
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;

use ax_proto::payloads::{
    BroadcastPayload, GetSharedPayload, ReadMessagesPayload, RecallMemoriesPayload,
    RememberMemoryPayload, SendMessagePayload, SetSharedPayload, SetStatusPayload,
};
use ax_proto::responses::{
    BroadcastResponse, GetSharedResponse, ListSharedResponse, ListWorkspacesResponse,
    MemoryResponse, ReadMessagesResponse, RecallMemoriesResponse, SendMessageResponse,
    StatusResponse,
};
use ax_proto::MessageType;

use crate::daemon_client::{DaemonClient, DaemonClientError};
use crate::memory_scope;

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
        }
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
    /// [`Self::with_config_path`]; callers who want Go-style CWD
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
}

impl Server {
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
}

#[allow(clippy::needless_pass_by_value)]
fn tool_error(err: DaemonClientError) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(err.to_string(), None)
}
