//! ax config loader.
//!
//! This is the Rust port of `internal/config`. Covered so far: the
//! core YAML schema (`Config`, `Workspace`, `Child`), path discovery,
//! recursive child loading with project-tree construction, and the
//! machine-managed overlay. Structural validation (duplicate names,
//! reserved-name collisions) is the remaining slice.

#![forbid(unsafe_code)]

mod overlay;
mod paths;
mod schema;
mod tree;

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
};
pub use tree::{ProjectNode, TreeError, WorkspaceRef};
