//! Per-workspace artifact authoring — the inverse of runtime tooling.
//!
//! First slice: `.mcp.json` merge/write/remove and marker-delimited
//! instruction section management for CLAUDE.md / AGENTS.md. These two
//! pieces are what `EnsureArtifacts` in `internal/workspace/workspace.go`
//! delegates to before creating tmux sessions, so once they land in
//! Rust the CLI can generate the same filesystem layout Go does today.
//!
//! Deferred to later slices: dispatch helpers and the reconcile/lifecycle
//! pass over the full workspace tree.

#![forbid(unsafe_code)]

mod instructions;
mod manager;
mod mcp_config;

pub use instructions::{remove_instructions, write_instructions, InstructionsError};
pub use manager::{
    cleanup_workspace_artifacts, ensure_artifacts, managed_run_agent_args, Manager, RealTmux,
    TmuxBackend, WorkspaceError,
};
pub use mcp_config::{remove_mcp_config, write_mcp_config, McpConfigError, MCP_CONFIG_FILE};
