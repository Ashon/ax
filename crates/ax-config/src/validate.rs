//! Structural validation for the ax config tree.
//!
//! Mirrors `internal/config/validate.go`. `validate_tree` does a pre-pass
//! over the entire project tree so duplicate child prefixes, duplicate
//! workspace directories, and reserved-name collisions with orchestrator
//! sessions fail loudly before `Config::load` tries to merge anything.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::overlay::ManagedOverlay;
use crate::paths::config_path_in_dir;
use crate::schema::Config;
use crate::tree::TreeError;

/// The kinds of structural failures a project tree can exhibit.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error(
        "duplicate ax child prefix {prefix:?}: child {first_name:?} in {first_path} -> {first_dir} conflicts with child {second_name:?} in {second_path} -> {second_dir}"
    )]
    DuplicateChildPrefix {
        prefix: String,
        first_name: String,
        first_path: String,
        first_dir: String,
        second_name: String,
        second_path: String,
        second_dir: String,
    },
    #[error(
        "duplicate ax workspace dir {dir:?}: workspace {first_name:?} in {first_path} (merged {first_merged:?}) conflicts with workspace {second_name:?} in {second_path} (merged {second_merged:?})"
    )]
    DuplicateWorkspaceDir {
        dir: String,
        first_name: String,
        first_path: String,
        first_merged: String,
        second_name: String,
        second_path: String,
        second_merged: String,
    },
    #[error(
        "reserved ax session name collision {merged:?}: workspace {name:?} in {path} conflicts with {existing}"
    )]
    ReservedNameForWorkspace {
        merged: String,
        name: String,
        path: String,
        existing: String,
    },
    #[error(
        "reserved ax session name collision {session:?}: child {child_name:?} in {path} -> {child_dir} (prefix {prefix:?}) conflicts with workspace {ws_name:?} in {ws_path}"
    )]
    ReservedNameForChild {
        session: String,
        child_name: String,
        path: String,
        child_dir: String,
        prefix: String,
        ws_name: String,
        ws_path: String,
    },
}

/// Run the validation pre-pass that `Config::load` / `Config::load_tree`
/// invoke before the merge phase.
pub fn validate_tree(path: impl AsRef<Path>) -> Result<(), TreeError> {
    let abs = absolutize(path.as_ref())?;
    let root_cfg = load_validated_config(&abs)?;

    let mut state = ValidationState::default();
    if !root_cfg.disable_root_orchestrator {
        state.orchestrators.insert(
            orchestrator_session_name(""),
            OrchestratorClaim {
                config_path: abs.clone(),
                child_name: String::new(),
                child_dir: PathBuf::new(),
                prefix: String::new(),
                session_name: orchestrator_session_name(""),
            },
        );
    }

    let mut seen = BTreeSet::new();
    validate_recursive(&abs, "", &mut seen, &mut state)
}

fn validate_recursive(
    path: &Path,
    prefix: &str,
    seen: &mut BTreeSet<PathBuf>,
    state: &mut ValidationState,
) -> Result<(), TreeError> {
    let abs = absolutize(path)?;
    if !seen.insert(abs.clone()) {
        return Err(TreeError::Cycle(abs));
    }

    let cfg = load_validated_config(&abs)?;
    let project_dir = crate::paths::ConfigRoot::from_config_path(&abs).0;

    let mut ws_names: Vec<_> = cfg.workspaces.keys().cloned().collect();
    ws_names.sort();
    for name in ws_names {
        let ws = cfg.workspaces.get(&name).cloned().unwrap_or_default();
        let merged_name = qualify_name(prefix, &name);
        if let Some(existing) = state.orchestrators.get(&merged_name) {
            return Err(TreeError::Validation(Box::new(
                ValidationError::ReservedNameForWorkspace {
                    merged: merged_name.clone(),
                    name: name.clone(),
                    path: abs.display().to_string(),
                    existing: describe_orchestrator(existing),
                },
            )));
        }
        if let Some(existing) = state.workspace_dirs.get(&ws.dir) {
            return Err(TreeError::Validation(Box::new(
                ValidationError::DuplicateWorkspaceDir {
                    dir: ws.dir.clone(),
                    first_name: existing.name.clone(),
                    first_path: existing.config_path.display().to_string(),
                    first_merged: existing.merged_name.clone(),
                    second_name: name.clone(),
                    second_path: abs.display().to_string(),
                    second_merged: merged_name.clone(),
                },
            )));
        }
        state
            .workspaces
            .entry(merged_name.clone())
            .or_insert_with(|| WorkspaceClaim {
                config_path: abs.clone(),
                name: name.clone(),
                merged_name: merged_name.clone(),
            });
        state.workspace_dirs.insert(
            ws.dir.clone(),
            WorkspaceDirClaim {
                config_path: abs.clone(),
                name: name.clone(),
                merged_name: merged_name.clone(),
                dir: ws.dir.clone(),
            },
        );
    }

    let mut child_names: Vec<_> = cfg.children.keys().cloned().collect();
    child_names.sort();
    for name in child_names {
        let mut child = cfg.children.get(&name).cloned().unwrap_or_default();
        let child_dir = resolve_dir(&project_dir, &child.dir);
        if child.prefix.is_empty() {
            child.prefix.clone_from(&name);
        }
        let Some(child_cfg_path) = config_path_in_dir(&child_dir) else {
            // Stale: skip without recording claims so a later re-validate
            // doesn't find phantom state.
            continue;
        };

        let claim_prefix = qualify_name(prefix, &child.prefix);
        if let Some(existing) = state.child_prefixes.get(&claim_prefix) {
            return Err(TreeError::Validation(Box::new(
                ValidationError::DuplicateChildPrefix {
                    prefix: claim_prefix.clone(),
                    first_name: existing.child_name.clone(),
                    first_path: existing.config_path.display().to_string(),
                    first_dir: existing.child_dir.display().to_string(),
                    second_name: name.clone(),
                    second_path: abs.display().to_string(),
                    second_dir: child_dir.display().to_string(),
                },
            )));
        }

        let orch_claim = OrchestratorClaim {
            config_path: abs.clone(),
            child_name: name.clone(),
            child_dir: child_dir.clone(),
            prefix: claim_prefix.clone(),
            session_name: orchestrator_session_name(&claim_prefix),
        };
        if let Some(existing) = state.workspaces.get(&orch_claim.session_name) {
            return Err(TreeError::Validation(Box::new(
                ValidationError::ReservedNameForChild {
                    session: orch_claim.session_name.clone(),
                    child_name: name.clone(),
                    path: abs.display().to_string(),
                    child_dir: child_dir.display().to_string(),
                    prefix: claim_prefix.clone(),
                    ws_name: existing.name.clone(),
                    ws_path: existing.config_path.display().to_string(),
                },
            )));
        }

        state.child_prefixes.insert(
            claim_prefix.clone(),
            ChildPrefixClaim {
                config_path: abs.clone(),
                child_name: name.clone(),
                child_dir: child_dir.clone(),
                prefix: claim_prefix.clone(),
            },
        );
        state
            .orchestrators
            .insert(orch_claim.session_name.clone(), orch_claim.clone());

        if let Err(err) = validate_recursive(&child_cfg_path, &claim_prefix, seen, state) {
            if is_missing_file(&err) {
                // Roll back claims — matches Go's behaviour for stale
                // descendents.
                state.child_prefixes.remove(&claim_prefix);
                state.orchestrators.remove(&orch_claim.session_name);
                continue;
            }
            return Err(TreeError::Child {
                name,
                dir: child_dir.display().to_string(),
                source: Box::new(err),
            });
        }
    }

    seen.remove(&abs);
    Ok(())
}

// ---------- state ----------

#[derive(Debug, Default)]
struct ValidationState {
    child_prefixes: BTreeMap<String, ChildPrefixClaim>,
    workspace_dirs: BTreeMap<String, WorkspaceDirClaim>,
    workspaces: BTreeMap<String, WorkspaceClaim>,
    orchestrators: BTreeMap<String, OrchestratorClaim>,
}

#[derive(Debug, Clone)]
struct ChildPrefixClaim {
    config_path: PathBuf,
    child_name: String,
    child_dir: PathBuf,
    #[allow(dead_code)]
    prefix: String,
}

#[derive(Debug, Clone)]
struct WorkspaceClaim {
    config_path: PathBuf,
    name: String,
    #[allow(dead_code)]
    merged_name: String,
}

#[derive(Debug, Clone)]
struct WorkspaceDirClaim {
    config_path: PathBuf,
    name: String,
    merged_name: String,
    #[allow(dead_code)]
    dir: String,
}

#[derive(Debug, Clone)]
struct OrchestratorClaim {
    config_path: PathBuf,
    child_name: String,
    child_dir: PathBuf,
    prefix: String,
    session_name: String,
}

// ---------- helpers (kept here so validate.rs doesn't need to reach
// into tree.rs for these private utilities) ----------

fn qualify_name(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    }
}

fn orchestrator_session_name(prefix: &str) -> String {
    if prefix.is_empty() {
        "orchestrator".to_owned()
    } else {
        format!("{prefix}.orchestrator")
    }
}

fn describe_orchestrator(claim: &OrchestratorClaim) -> String {
    if claim.child_name.is_empty() {
        format!("the root orchestrator in {}", claim.config_path.display())
    } else {
        format!(
            "child {name:?} in {cfg} -> {dir} (prefix {pfx:?}, orchestrator {orch:?})",
            name = claim.child_name,
            cfg = claim.config_path.display(),
            dir = claim.child_dir.display(),
            pfx = claim.prefix,
            orch = claim.session_name,
        )
    }
}

fn resolve_dir(base: &Path, value: &str) -> PathBuf {
    let value = if value.is_empty() { "." } else { value };
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    let p = Path::new(value);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn absolutize(path: &Path) -> Result<PathBuf, TreeError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|e| crate::schema::LoadError::Read {
        path: path.display().to_string(),
        source: e,
    })?;
    Ok(cwd.join(path))
}

fn load_validated_config(abs: &Path) -> Result<Config, TreeError> {
    let mut cfg = Config::read_local(abs)?;
    // validate pass mirrors Go's loadLocalConfig: initialize + overlay +
    // normalize so validation sees the same merged view Config::load will.
    if cfg.project.is_empty() {
        let root = crate::paths::ConfigRoot::from_config_path(abs).0;
        let base = root.file_name().and_then(|os| os.to_str()).unwrap_or("");
        base.clone_into(&mut cfg.project);
    }
    if cfg.experimental_mcp_team_reconfigure {
        let overlay = ManagedOverlay::load_for(abs)?;
        overlay.apply_to(&mut cfg);
    }
    let project_dir = crate::paths::ConfigRoot::from_config_path(abs).0;
    let fallback_effort = cfg.codex_model_reasoning_effort.trim().to_owned();
    let ws_names: Vec<_> = cfg.workspaces.keys().cloned().collect();
    for name in ws_names {
        if let Some(ws) = cfg.workspaces.get_mut(&name) {
            ws.dir = resolve_dir(&project_dir, &ws.dir)
                .to_string_lossy()
                .into_owned();
            if ws.codex_model_reasoning_effort.trim().is_empty() {
                ws.codex_model_reasoning_effort.clone_from(&fallback_effort);
            }
        }
    }
    let child_names: Vec<_> = cfg.children.keys().cloned().collect();
    for name in child_names {
        if let Some(child) = cfg.children.get_mut(&name) {
            child.dir = resolve_dir(&project_dir, &child.dir)
                .to_string_lossy()
                .into_owned();
            if child.prefix.is_empty() {
                child.prefix.clone_from(&name);
            }
        }
    }
    Ok(cfg)
}

fn is_missing_file(err: &TreeError) -> bool {
    if let TreeError::Load(crate::schema::LoadError::Read { source, .. }) = err {
        source.kind() == std::io::ErrorKind::NotFound
    } else {
        false
    }
}
