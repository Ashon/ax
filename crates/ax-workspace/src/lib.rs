//! Per-workspace artifact authoring and lifecycle control.
//!
//! Covers:
//! - `.mcp.json` merge/write/remove and marker-delimited
//!   instruction-section management for `CLAUDE.md` / `AGENTS.md`.
//! - `ensure_artifacts` + [`Manager`] (create/restart/destroy)
//!   generating the on-disk layout tmux sessions expect.
//! - [`Reconciler`] and lifecycle helpers that drive workspace /
//!   sub-orchestrator convergence against `Config`.

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
    dispatch_runnable_work, dispatch_runnable_work_with_options, enforce_capacity_cap,
    ensure_dispatch_target, load_dispatch_desired_state, DispatchBackend, DispatchError,
    DispatchOptions,
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
