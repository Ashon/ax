//! ax config loader.
//!
//! Covers the full config surface: YAML schema (`Config`, `Workspace`,
//! `Child`), path discovery, recursive child loading with project-tree
//! construction, machine-managed overlay merging, and structural
//! validation.

#![forbid(unsafe_code)]

mod overlay;
mod paths;
mod schema;
mod tree;
mod validate;

pub use overlay::{
    managed_overlay_path, ManagedChildPatch, ManagedOverlay, ManagedPolicyOverlay,
    ManagedWorkspacePatch, MANAGED_OVERLAY_FILE,
};
pub use paths::{
    config_path_in_dir, default_config_path, find_config_file, legacy_config_path, ConfigRoot,
    DEFAULT_CONFIG_DIR, DEFAULT_CONFIG_FILE, LEGACY_CONFIG_FILE,
};
pub use schema::{
    default_idle_timeout_minutes, Child, Config, LoadError, Workspace,
    DEFAULT_CODEX_REASONING_EFFORT, DEFAULT_IDLE_TIMEOUT_MINUTES,
    DEFAULT_MAX_CHILDREN_PER_NODE, DEFAULT_MAX_CONCURRENT_AGENTS,
    DEFAULT_MAX_ORCHESTRATOR_DEPTH,
};
pub use tree::{ProjectNode, TreeError, WorkspaceRef};
pub use validate::{validate_tree, ValidationError};
