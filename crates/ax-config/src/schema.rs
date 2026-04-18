//! Config YAML schema. Mirrors `Config` / `Workspace` / `Child` from
//! `internal/config/config.go`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const DEFAULT_CODEX_REASONING_EFFORT: &str = "xhigh";
pub const DEFAULT_IDLE_TIMEOUT_MINUTES: i32 = 15;

/// Cap on how deep the child-orchestrator tree can recurse. Root is
/// depth 0; a child loaded from root is depth 1. Zero means
/// unbounded — use only when you actively want that risk.
pub const DEFAULT_MAX_ORCHESTRATOR_DEPTH: u32 = 3;

/// Cap on how many children one config node may declare. Zero means
/// unbounded.
pub const DEFAULT_MAX_CHILDREN_PER_NODE: u32 = 6;

pub fn default_idle_timeout_minutes() -> i32 {
    DEFAULT_IDLE_TIMEOUT_MINUTES
}

pub(crate) fn default_max_orchestrator_depth() -> u32 {
    DEFAULT_MAX_ORCHESTRATOR_DEPTH
}

pub(crate) fn default_max_children_per_node() -> u32 {
    DEFAULT_MAX_CHILDREN_PER_NODE
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("read config {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse config {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_yml::Error,
    },
    #[error("serialize config: {0}")]
    Serialize(#[from] serde_yml::Error),
    #[error("write config {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Root ax config file schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub project: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub orchestrator_runtime: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub disable_root_orchestrator: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub experimental_mcp_team_reconfigure: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub codex_model_reasoning_effort: String,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub idle_timeout_minutes: i32,
    /// Max recursion depth for child orchestrators; only honoured on
    /// the root config. Zero disables the check.
    #[serde(
        default = "default_max_orchestrator_depth",
        skip_serializing_if = "is_default_orchestrator_depth"
    )]
    pub max_orchestrator_depth: u32,
    /// Max number of `children:` entries a single config may declare.
    /// Zero disables the check.
    #[serde(
        default = "default_max_children_per_node",
        skip_serializing_if = "is_default_children_per_node"
    )]
    pub max_children_per_node: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub children: BTreeMap<String, Child>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspaces: BTreeMap<String, Workspace>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Child {
    pub dir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Workspace {
    pub dir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shell: String,
    /// `claude` or `codex`. Empty string defaults to `claude` at runtime.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runtime: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub codex_model_reasoning_effort: String,
    /// Custom command that replaces the runtime default when non-empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    /// Agent instructions written to the runtime's instruction file.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            project: String::new(),
            orchestrator_runtime: String::new(),
            disable_root_orchestrator: false,
            experimental_mcp_team_reconfigure: false,
            codex_model_reasoning_effort: String::new(),
            idle_timeout_minutes: 0,
            max_orchestrator_depth: DEFAULT_MAX_ORCHESTRATOR_DEPTH,
            max_children_per_node: DEFAULT_MAX_CHILDREN_PER_NODE,
            children: BTreeMap::new(),
            workspaces: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Returns the idle timeout in minutes, using the ax default (15) when
    /// the field is unset or non-positive.
    #[must_use]
    pub fn idle_timeout_minutes_or_default(&self) -> i32 {
        if self.idle_timeout_minutes > 0 {
            self.idle_timeout_minutes
        } else {
            DEFAULT_IDLE_TIMEOUT_MINUTES
        }
    }

    /// Parse a config from a YAML string. Does not resolve children or
    /// apply normalization; callers wanting the full `internal/config`
    /// behaviour should use the (forthcoming) `load` entry point.
    pub fn from_yaml(source: &str) -> Result<Self, serde_yml::Error> {
        serde_yml::from_str(source)
    }

    /// Read and parse a single YAML file without any recursive resolution.
    pub fn read_local(path: &Path) -> Result<Self, LoadError> {
        let data = std::fs::read_to_string(path).map_err(|e| LoadError::Read {
            path: path.display().to_string(),
            source: e,
        })?;
        serde_yml::from_str(&data).map_err(|e| LoadError::Parse {
            path: path.display().to_string(),
            source: e,
        })
    }

    /// Serialize to YAML and write to `path`, creating parent directories.
    pub fn save(&self, path: &Path) -> Result<(), LoadError> {
        let data = serde_yml::to_string(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LoadError::Write {
                path: path.display().to_string(),
                source: e,
            })?;
        }
        std::fs::write(path, data).map_err(|e| LoadError::Write {
            path: path.display().to_string(),
            source: e,
        })
    }

    /// Builds a Config matching `DefaultConfigForRuntime` in Go — used by
    /// `ax init`-style flows.
    #[must_use]
    pub fn default_for_runtime(project_name: &str, runtime: &str) -> Self {
        let mut workspaces = BTreeMap::new();
        workspaces.insert(
            "main".to_owned(),
            Workspace {
                dir: ".".to_owned(),
                description: "Main workspace".to_owned(),
                runtime: runtime.to_owned(),
                codex_model_reasoning_effort: DEFAULT_CODEX_REASONING_EFFORT.to_owned(),
                ..Default::default()
            },
        );
        Self {
            project: project_name.to_owned(),
            orchestrator_runtime: runtime.to_owned(),
            codex_model_reasoning_effort: DEFAULT_CODEX_REASONING_EFFORT.to_owned(),
            idle_timeout_minutes: DEFAULT_IDLE_TIMEOUT_MINUTES,
            workspaces,
            ..Default::default()
        }
    }
}

// serde's `skip_serializing_if` requires `fn(&T) -> bool`; clippy's
// trivially_copy_pass_by_ref wants value-level, but we're bound by serde.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(v: &bool) -> bool {
    !*v
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_default_orchestrator_depth(v: &u32) -> bool {
    *v == DEFAULT_MAX_ORCHESTRATOR_DEPTH
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_default_children_per_node(v: &u32) -> bool {
    *v == DEFAULT_MAX_CHILDREN_PER_NODE
}
