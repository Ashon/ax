//! Memory scope resolver for the MCP tool handlers. Accepts the scope
//! aliases `workspace`, `project`, and `global`, plus explicit
//! selectors of the form `project:x`, `workspace:y`, `task:<id>`.

use std::path::{Path, PathBuf};

use ax_config::{find_config_file, Config, ProjectNode};
use ax_workspace::orchestrator_name;

pub(crate) const GLOBAL_SCOPE: &str = "global";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ScopeError {
    #[error(
        "invalid memory scope {0:?}; use `workspace`, `project`, `global`, or an explicit selector"
    )]
    Invalid(String),
    #[error("config file not found when resolving project scope")]
    ConfigNotFound,
    #[error("load config tree {path}: {source}")]
    LoadTree {
        path: String,
        #[source]
        source: ax_config::TreeError,
    },
    #[error("workspace {workspace:?} not found in config tree {path}")]
    WorkspaceNotInTree { workspace: String, path: String },
}

#[must_use]
pub(crate) fn workspace_scope(workspace: &str) -> String {
    format!("workspace:{}", workspace.trim())
}

#[must_use]
pub(crate) fn project_scope(prefix: &str) -> String {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        "project:root".to_owned()
    } else {
        format!("project:{trimmed}")
    }
}

/// Normalise a raw scope string so case and whitespace match the
/// scope the daemon already stores in the memory store.
pub(crate) fn normalize_scope(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.eq_ignore_ascii_case("global") {
        return GLOBAL_SCOPE.to_owned();
    }
    for prefix in ["project:", "workspace:", "task:"] {
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with(prefix) {
            let value = trimmed[prefix.len()..].trim();
            if prefix == "project:" {
                let v = if value.is_empty() { "root" } else { value };
                return format!("project:{v}");
            }
            if value.is_empty() {
                return String::new();
            }
            return format!("{prefix}{value}");
        }
    }
    trimmed.to_owned()
}

/// Resolve one raw selector (possibly the alias `workspace`,
/// `project`, or `global`) into the canonical scope string the
/// daemon stores. Requires the server's workspace name + the
/// effective config path for `project` lookups.
pub(crate) fn resolve(
    raw: &str,
    workspace: &str,
    effective_config: Option<&Path>,
) -> Result<String, ScopeError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("workspace") {
        return Ok(workspace_scope(workspace));
    }
    if trimmed.eq_ignore_ascii_case("global") {
        return Ok(GLOBAL_SCOPE.to_owned());
    }
    if trimmed.eq_ignore_ascii_case("project") {
        return resolve_project(workspace, effective_config);
    }
    let scope = normalize_scope(trimmed);
    if scope == GLOBAL_SCOPE
        || scope.starts_with("project:")
        || scope.starts_with("workspace:")
        || scope.starts_with("task:")
    {
        return Ok(scope);
    }
    Err(ScopeError::Invalid(raw.to_owned()))
}

/// Resolve a list of scopes, deduping while preserving insertion
/// order. Empty input falls back to `[global, project, workspace]`.
pub(crate) fn resolve_many(
    scopes: &[String],
    workspace: &str,
    effective_config: Option<&Path>,
) -> Result<Vec<String>, ScopeError> {
    let owned: Vec<&str>;
    let defaults = ["global", "project", "workspace"];
    let iter: &[&str] = if scopes.is_empty() {
        &defaults
    } else {
        owned = scopes.iter().map(String::as_str).collect();
        &owned
    };
    let mut out = Vec::with_capacity(iter.len());
    for raw in iter {
        let resolved = resolve(raw, workspace, effective_config)?;
        if !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    Ok(out)
}

fn resolve_project(workspace: &str, effective_config: Option<&Path>) -> Result<String, ScopeError> {
    let path = effective_config
        .map(Path::to_path_buf)
        .or_else(|| find_config_file(std::env::current_dir().ok()?))
        .ok_or(ScopeError::ConfigNotFound)?;
    let tree = Config::load_tree(&path).map_err(|source| ScopeError::LoadTree {
        path: path.display().to_string(),
        source,
    })?;
    let prefix =
        find_project_prefix(&tree, workspace).ok_or_else(|| ScopeError::WorkspaceNotInTree {
            workspace: workspace.to_owned(),
            path: path.display().to_string(),
        })?;
    Ok(project_scope(&prefix))
}

fn find_project_prefix(node: &ProjectNode, target: &str) -> Option<String> {
    if orchestrator_name(&node.prefix) == target {
        return Some(node.prefix.clone());
    }
    for ws in &node.workspaces {
        if ws.merged_name == target {
            return Some(node.prefix.clone());
        }
    }
    for child in &node.children {
        if let Some(prefix) = find_project_prefix(child, target) {
            return Some(prefix);
        }
    }
    None
}

/// Best-effort config-path resolution for the ax-cli entry point.
/// Uses the caller-provided path when set, otherwise walks up from
/// `$CWD` looking for `.ax/config.yaml`. Library callers (tests,
/// embedding contexts) should typically pass `configured =
/// Some(path)` explicitly so the process-global CWD is never
/// consulted.
#[must_use]
pub fn find_effective_config(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return Some(path.to_path_buf());
    }
    let cwd = std::env::current_dir().ok()?;
    find_config_file(cwd)
}
