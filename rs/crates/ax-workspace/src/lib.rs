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

mod dispatch;
mod instructions;
mod lifecycle;
mod manager;
mod mcp_config;
mod orchestrator;
mod orchestrator_prompt;
mod reconcile;

pub use dispatch::{
    dispatch_runnable_work, dispatch_runnable_work_with_options, ensure_dispatch_target,
    load_dispatch_desired_state, DispatchBackend, DispatchError, DispatchOptions,
};
pub use instructions::{remove_instructions, write_instructions, InstructionsError};
pub use lifecycle::{restart_named_target, start_named_target, stop_named_target, LifecycleError};
pub use manager::{
    cleanup_workspace_artifacts, cleanup_workspace_state, ensure_artifacts, managed_run_agent_args,
    Manager, RealTmux, TmuxBackend, WorkspaceError,
};
pub use mcp_config::{remove_mcp_config, write_mcp_config, McpConfigError, MCP_CONFIG_FILE};
pub use orchestrator::{
    cleanup_orchestrator_artifacts, cleanup_orchestrator_state, ensure_orchestrator,
    ensure_orchestrator_tree, orchestrator_dir_for_node, orchestrator_name, root_orchestrator_dir,
    OrchestratorError,
};
pub use orchestrator_prompt::{
    orchestrator_prompt, write_orchestrator_prompt, OrchestratorPromptError,
};
pub use reconcile::{
    build_desired_state, build_desired_state_with_tree, DesiredOrchestrator, DesiredState,
    DesiredWorkspace, OrchestratorState, ReconcileAction, ReconcileError, ReconcileOptions,
    ReconcileReport, Reconciler, WorkspaceState, RUNTIME_STATE_FILE,
};
