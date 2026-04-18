//! MCP server scaffold + tool registrations that delegate to the
//! daemon client. Mirrors `internal/mcpserver/server.go` +
//! `tools_shared.go` + `tools_workspace.go` for the first slice.
//!
//! Additional tool groups (memory, usage, messages, tasks, agents,
//! `team_reconfigure`) land in follow-up commits. The per-tool
//! behaviour is wire-compatible with Go: each tool body calls into
//! the `DaemonClient` using the same envelope types + payloads, then
//! returns JSON-formatted text via MCP `CallToolResult::structured`.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::service::RunningService;
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;

use ax_proto::payloads::{GetSharedPayload, SetSharedPayload, SetStatusPayload};
use ax_proto::responses::{
    GetSharedResponse, ListSharedResponse, ListWorkspacesResponse, StatusResponse,
};
use ax_proto::MessageType;

use crate::daemon_client::{DaemonClient, DaemonClientError};

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
/// the daemon client and the generated tool router. Clone is cheap
/// because the client is `Arc`-based.
#[derive(Clone)]
pub struct Server {
    daemon: DaemonClient,
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
            tool_router: Self::tool_router(),
        }
    }

    #[must_use]
    pub fn daemon(&self) -> &DaemonClient {
        &self.daemon
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
