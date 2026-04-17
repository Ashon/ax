//! Recursive `Config` / `ProjectNode` loading.
//!
//! Mirrors `internal/config/config.go::Load` (workspace-merged view) and
//! `internal/config/tree.go::LoadTree` (hierarchical view). Stale child
//! references where the target `.ax/config.yaml` no longer exists are
//! skipped with a warning-like log, matching the Go behaviour so the rest
//! of the tree keeps loading.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::paths::{config_path_in_dir, ConfigRoot};
use crate::schema::{Child, Config, LoadError, Workspace};

#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("cyclic ax children reference at {0}")]
    Cycle(PathBuf),
    #[error("load child {name} at {dir}: {source}")]
    Child {
        name: String,
        dir: String,
        #[source]
        source: Box<TreeError>,
    },
}

impl Config {
    /// Recursive load, merging child-project workspaces into the root
    /// `Config.workspaces` map with `prefix.name` keys.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, TreeError> {
        let mut seen = BTreeSet::new();
        load_recursive(path.as_ref(), &mut seen)
    }

    /// Like [`Config::load`] but preserves the hierarchy instead of
    /// flattening workspaces.
    pub fn load_tree(path: impl AsRef<Path>) -> Result<ProjectNode, TreeError> {
        let mut seen = BTreeSet::new();
        load_tree_recursive(path.as_ref(), "", &mut seen)
    }
}

fn load_recursive(path: &Path, seen: &mut BTreeSet<PathBuf>) -> Result<Config, TreeError> {
    let abs = absolutize(path)?;
    if !seen.insert(abs.clone()) {
        return Err(TreeError::Cycle(abs));
    }

    let mut cfg = Config::read_local(&abs)?;
    initialize_local(&mut cfg, &abs);
    normalize_local(&mut cfg, &abs);

    let mut child_names: Vec<_> = cfg.children.keys().cloned().collect();
    child_names.sort();
    for name in child_names {
        let child = cfg.children.get(&name).cloned().unwrap_or_default();
        let Some(child_path) = config_path_in_dir(&child.dir) else {
            eprintln!(
                "warning: child {name:?} at {} has no config, skipping",
                child.dir
            );
            continue;
        };
        let child_cfg = match load_recursive(&child_path, seen) {
            Ok(v) => v,
            Err(e) if is_missing_file(&e) => {
                eprintln!(
                    "warning: child {name:?} at {} has no config, skipping",
                    child.dir
                );
                continue;
            }
            Err(e) => {
                return Err(TreeError::Child {
                    name,
                    dir: child.dir,
                    source: Box::new(e),
                });
            }
        };
        for (ws_name, ws) in child_cfg.workspaces {
            let merged = qualify_name(&child.prefix, &ws_name);
            if cfg.workspaces.contains_key(&merged) {
                eprintln!("warning: duplicate workspace {merged:?} from child {name:?}, skipping");
                continue;
            }
            cfg.workspaces.insert(merged, ws);
        }
    }

    seen.remove(&abs);
    Ok(cfg)
}

fn load_tree_recursive(
    path: &Path,
    prefix: &str,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<ProjectNode, TreeError> {
    let abs = absolutize(path)?;
    if !seen.insert(abs.clone()) {
        return Err(TreeError::Cycle(abs));
    }

    let mut cfg = Config::read_local(&abs)?;
    initialize_local(&mut cfg, &abs);
    normalize_local(&mut cfg, &abs);

    let mut node = ProjectNode {
        name: cfg.project.clone(),
        alias: String::new(),
        prefix: prefix.to_owned(),
        dir: ConfigRoot::from_config_path(&abs).0,
        orchestrator_runtime: cfg.orchestrator_runtime.clone(),
        disable_root_orchestrator: prefix.is_empty() && cfg.disable_root_orchestrator,
        workspaces: Vec::new(),
        children: Vec::new(),
    };

    let mut ws_names: Vec<_> = cfg.workspaces.keys().cloned().collect();
    ws_names.sort();
    for name in ws_names {
        let ws = cfg.workspaces.get(&name).cloned().unwrap_or_default();
        let merged = qualify_name(prefix, &name);
        node.workspaces.push(WorkspaceRef {
            name,
            merged_name: merged,
            runtime: ws.runtime,
            description: ws.description,
            instructions: ws.instructions,
        });
    }

    let mut child_names: Vec<_> = cfg.children.keys().cloned().collect();
    child_names.sort();
    for name in child_names {
        let child = cfg.children.get(&name).cloned().unwrap_or_default();
        let child_prefix = qualify_name(prefix, &child.prefix);
        let Some(child_path) = config_path_in_dir(&child.dir) else {
            continue;
        };
        let child_node = match load_tree_recursive(&child_path, &child_prefix, seen) {
            Ok(v) => v,
            Err(e) if is_missing_file(&e) => continue,
            Err(e) => {
                return Err(TreeError::Child {
                    name,
                    dir: child.dir,
                    source: Box::new(e),
                });
            }
        };
        let mut child_node = child_node;
        child_node.alias = name;
        node.children.push(child_node);
    }

    seen.remove(&abs);
    Ok(node)
}

/// One project in the ax hierarchy — the tree form preserved by
/// `Config::load_tree`.
#[derive(Debug, Clone, Default)]
pub struct ProjectNode {
    pub name: String,
    /// Mount alias used by the parent's `children` map (empty at the root).
    pub alias: String,
    /// Fully-qualified prefix (e.g. `"team.sub"`) used when merging
    /// workspace names into the parent scope.
    pub prefix: String,
    pub dir: PathBuf,
    pub orchestrator_runtime: String,
    pub disable_root_orchestrator: bool,
    pub workspaces: Vec<WorkspaceRef>,
    pub children: Vec<ProjectNode>,
}

impl ProjectNode {
    #[must_use]
    pub fn display_name(&self) -> String {
        if self.alias.is_empty() || self.alias == self.name {
            self.name.clone()
        } else {
            format!("{} ({})", self.alias, self.name)
        }
    }
}

/// Workspace membership inside a [`ProjectNode`]. `merged_name` is the
/// name the daemon + tmux use at runtime.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceRef {
    pub name: String,
    pub merged_name: String,
    pub runtime: String,
    pub description: String,
    pub instructions: String,
}

// ---------- helpers ----------

fn qualify_name(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    }
}

fn initialize_local(cfg: &mut Config, path: &Path) {
    if cfg.project.is_empty() {
        let root = ConfigRoot::from_config_path(path).0;
        let base = root.file_name().and_then(|os| os.to_str()).unwrap_or("");
        base.clone_into(&mut cfg.project);
    }
}

fn normalize_local(cfg: &mut Config, path: &Path) {
    let project_dir = ConfigRoot::from_config_path(path).0;

    let fallback_effort = cfg.codex_model_reasoning_effort.trim().to_owned();
    let workspace_names: Vec<_> = cfg.workspaces.keys().cloned().collect();
    for name in workspace_names {
        if let Some(ws) = cfg.workspaces.get_mut(&name) {
            ws.dir = resolve_dir(&project_dir, &ws.dir);
            if ws.codex_model_reasoning_effort.trim().is_empty() {
                ws.codex_model_reasoning_effort.clone_from(&fallback_effort);
            }
        }
    }

    let child_names: Vec<_> = cfg.children.keys().cloned().collect();
    for name in child_names {
        if let Some(child) = cfg.children.get_mut(&name) {
            child.dir = resolve_dir(&project_dir, &child.dir);
            if child.prefix.is_empty() {
                child.prefix.clone_from(&name);
            }
        }
    }
    let _ = Workspace::default(); // keep the re-export live to users
}

fn resolve_dir(base: &Path, value: &str) -> String {
    let value = if value.is_empty() { "." } else { value };
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    let p = Path::new(value);
    if p.is_absolute() {
        value.to_owned()
    } else {
        base.join(p).to_string_lossy().into_owned()
    }
}

fn absolutize(path: &Path) -> Result<PathBuf, TreeError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|e| LoadError::Read {
        path: path.display().to_string(),
        source: e,
    })?;
    Ok(cwd.join(path))
}

fn is_missing_file(err: &TreeError) -> bool {
    if let TreeError::Load(LoadError::Read { source, .. }) = err {
        source.kind() == std::io::ErrorKind::NotFound
    } else {
        false
    }
}

#[allow(dead_code)]
fn _keep_children_type_live(_m: &BTreeMap<String, Child>) {}
