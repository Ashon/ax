//! ax config loader.
//!
//! This is the Rust port of `internal/config`. Initial scope is the core
//! YAML structure (`Config`, `Workspace`, `Child`) plus path discovery
//! helpers. Recursive child loading, managed overlay, and validation are
//! covered by follow-up commits.

#![forbid(unsafe_code)]

mod paths;
mod schema;
mod tree;

pub use paths::{
    config_path_in_dir, default_config_path, find_config_file, legacy_config_path, ConfigRoot,
    DEFAULT_CONFIG_DIR, DEFAULT_CONFIG_FILE, LEGACY_CONFIG_FILE,
};
pub use schema::{
    default_idle_timeout_minutes, Child, Config, LoadError, Workspace,
    DEFAULT_CODEX_REASONING_EFFORT, DEFAULT_IDLE_TIMEOUT_MINUTES,
};
pub use tree::{ProjectNode, TreeError, WorkspaceRef};
