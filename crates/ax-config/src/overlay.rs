//! Machine-managed overlay merged on top of the user-authored
//! `config.yaml` when the experimental team-reconfigure feature is on.
//! Mirrors `internal/config/managed_overlay.go`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::{ConfigRoot, DEFAULT_CONFIG_DIR};
use crate::schema::{Config, LoadError};

pub const MANAGED_OVERLAY_FILE: &str = "managed_overlay.yaml";

/// Persisted overlay that ax edits programmatically (via MCP team
/// reconfigure tools) without rewriting the user's YAML.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagedOverlay {
    #[serde(default, skip_serializing_if = "managed_policy_is_empty")]
    pub policies: ManagedPolicyOverlay,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspaces: BTreeMap<String, ManagedWorkspacePatch>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub children: BTreeMap<String, ManagedChildPatch>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagedPolicyOverlay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_root_orchestrator: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagedWorkspacePatch {
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagedChildPatch {
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Compute the path where the overlay for `config_path` lives. Matches
/// `ManagedOverlayPath` in Go: always under `<project>/.ax/` regardless
/// of whether the user picked the legacy flat layout.
#[must_use]
pub fn managed_overlay_path(config_path: impl AsRef<Path>) -> PathBuf {
    let abs = absolutize(config_path.as_ref());
    ConfigRoot::from_config_path(&abs)
        .0
        .join(DEFAULT_CONFIG_DIR)
        .join(MANAGED_OVERLAY_FILE)
}

impl ManagedOverlay {
    /// Read the overlay adjacent to `config_path`. Missing file is
    /// silently returned as the empty overlay, matching Go.
    pub fn load_for(config_path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let path = managed_overlay_path(config_path);
        let data = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(LoadError::Read {
                    path: path.display().to_string(),
                    source: e,
                });
            }
        };
        serde_yml::from_str(&data).map_err(|e| LoadError::Parse {
            path: path.display().to_string(),
            source: e,
        })
    }

    /// Serialize and write the overlay for `config_path`, creating the
    /// `.ax/` directory if needed.
    pub fn save_for(&self, config_path: impl AsRef<Path>) -> Result<(), LoadError> {
        let path = managed_overlay_path(config_path);
        let data = serde_yml::to_string(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LoadError::Write {
                path: path.display().to_string(),
                source: e,
            })?;
        }
        std::fs::write(&path, data).map_err(|e| LoadError::Write {
            path: path.display().to_string(),
            source: e,
        })
    }

    /// Patch `cfg` in place, matching `applyManagedOverlay` in Go.
    /// Fields set to `Some(_)` win; `delete: true` or `enabled: Some(false)`
    /// removes the entry entirely.
    pub fn apply_to(&self, cfg: &mut Config) {
        if let Some(runtime) = &self.policies.orchestrator_runtime {
            cfg.orchestrator_runtime.clone_from(runtime);
        }
        if let Some(flag) = self.policies.disable_root_orchestrator {
            cfg.disable_root_orchestrator = flag;
        }

        // Workspaces: delete, then patch. Iterate a snapshot of names so
        // we can mutate freely inside the loop.
        let ws_names: Vec<_> = self.workspaces.keys().cloned().collect();
        for name in ws_names {
            let patch = &self.workspaces[&name];
            if patch.delete || is_disabled(patch.enabled) {
                cfg.workspaces.remove(&name);
                continue;
            }
            let entry = cfg.workspaces.entry(name).or_default();
            if let Some(v) = &patch.dir {
                entry.dir.clone_from(v);
            }
            if let Some(v) = &patch.description {
                entry.description.clone_from(v);
            }
            if let Some(v) = &patch.runtime {
                entry.runtime.clone_from(v);
            }
            if let Some(v) = &patch.shell {
                entry.shell.clone_from(v);
            }
            if let Some(v) = &patch.agent {
                entry.agent.clone_from(v);
            }
        }

        let child_names: Vec<_> = self.children.keys().cloned().collect();
        for name in child_names {
            let patch = &self.children[&name];
            if patch.delete || is_disabled(patch.enabled) {
                cfg.children.remove(&name);
                continue;
            }
            let entry = cfg.children.entry(name).or_default();
            if let Some(v) = &patch.dir {
                entry.dir.clone_from(v);
            }
            if let Some(v) = &patch.prefix {
                entry.prefix.clone_from(v);
            }
        }
    }
}

fn is_disabled(enabled: Option<bool>) -> bool {
    matches!(enabled, Some(false))
}

fn managed_policy_is_empty(p: &ManagedPolicyOverlay) -> bool {
    p.orchestrator_runtime.is_none() && p.disable_root_orchestrator.is_none()
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(v: &bool) -> bool {
    !*v
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}
