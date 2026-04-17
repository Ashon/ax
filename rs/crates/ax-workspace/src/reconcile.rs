//! Workspace + orchestrator reconcile pass.
//!
//! This ports the generated-artifact and managed-session half of
//! `internal/workspace/reconcile.go`: persisted runtime state,
//! desired-vs-actual diffing, create/remove/restart for workspaces and
//! sub-orchestrators, and root-orchestrator manual-restart reporting.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use ax_agent::Runtime;
use ax_config::{Config, ProjectNode, Workspace};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::{
    cleanup_orchestrator_state, cleanup_workspace_state, ensure_artifacts, ensure_orchestrator,
    orchestrator_dir_for_node, orchestrator_name, orchestrator_prompt, Manager, OrchestratorError,
    RealTmux, TmuxBackend, WorkspaceError,
};

pub const RUNTIME_STATE_FILE: &str = ".runtime-state.json";
const RUNTIME_STATE_VERSION: i32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error("read reconcile state {path}: {source}")]
    ReadState {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("parse reconcile state {path}: {source}")]
    ParseState {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("encode reconcile state: {0}")]
    EncodeState(#[from] serde_json::Error),
    #[error("write reconcile state {path}: {source}")]
    WriteState {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Orchestrator(#[from] OrchestratorError),
    #[error(transparent)]
    Tmux(#[from] ax_tmux::TmuxError),
}

#[derive(Debug, Clone, Default)]
pub struct ReconcileOptions {
    pub daemon_running: bool,
    pub allow_disruptive_changes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileAction {
    pub kind: String,
    pub name: String,
    pub operation: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub details: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileReport {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ReconcileAction>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub root_manual_restart_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub root_manual_restart_reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DesiredWorkspace {
    pub name: String,
    pub workspace: Workspace,
}

#[derive(Debug, Clone)]
pub struct DesiredOrchestrator {
    pub name: String,
    pub node: ProjectNode,
    pub parent_name: String,
    pub artifact_dir: PathBuf,
    pub runtime: String,
    pub root: bool,
    pub managed_session: bool,
    pub prompt_hash: String,
}

#[derive(Debug, Clone, Default)]
pub struct DesiredState {
    pub socket_path: PathBuf,
    pub config_path: PathBuf,
    pub workspaces: BTreeMap<String, DesiredWorkspace>,
    pub orchestrators: BTreeMap<String, DesiredOrchestrator>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub name: String,
    pub dir: String,
    pub runtime: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shell: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestratorState {
    pub name: String,
    pub artifact_dir: String,
    pub runtime: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt_hash: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub managed_session: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub root: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeState {
    version: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    socket_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    config_path: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspaces: BTreeMap<String, WorkspaceState>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    orchestrators: BTreeMap<String, OrchestratorState>,
}

#[derive(Debug, Clone, Default)]
struct SessionSnapshot {
    exists: bool,
    attached: bool,
    idle: bool,
}

pub struct Reconciler<B = RealTmux> {
    socket_path: PathBuf,
    config_path: PathBuf,
    ax_bin: PathBuf,
    tmux: B,
    manager: Manager<B>,
}

impl Reconciler<RealTmux> {
    #[must_use]
    pub fn new(
        socket_path: impl Into<PathBuf>,
        config_path: impl Into<PathBuf>,
        ax_bin: impl Into<PathBuf>,
    ) -> Self {
        let socket_path = socket_path.into();
        let config_path = config_path.into();
        let ax_bin = ax_bin.into();
        let tmux = RealTmux;
        let manager = Manager::with_tmux(
            socket_path.clone(),
            Some(config_path.clone()),
            ax_bin.clone(),
            tmux,
        );
        Self {
            socket_path,
            config_path,
            ax_bin,
            tmux,
            manager,
        }
    }
}

impl<B: TmuxBackend + Clone> Reconciler<B> {
    #[must_use]
    pub fn with_tmux(
        socket_path: impl Into<PathBuf>,
        config_path: impl Into<PathBuf>,
        ax_bin: impl Into<PathBuf>,
        tmux: B,
    ) -> Self {
        let socket_path = socket_path.into();
        let config_path = config_path.into();
        let ax_bin = ax_bin.into();
        let manager = Manager::with_tmux(
            socket_path.clone(),
            Some(config_path.clone()),
            ax_bin.clone(),
            tmux.clone(),
        );
        Self {
            socket_path,
            config_path,
            ax_bin,
            tmux,
            manager,
        }
    }

    pub fn reconcile_desired_state(
        &self,
        desired: &DesiredState,
        opts: ReconcileOptions,
    ) -> Result<ReconcileReport, ReconcileError> {
        let state_path = reconcile_state_path(&self.config_path);
        let previous = load_runtime_state(&state_path)?;
        let sessions =
            load_session_snapshots(&self.tmux, desired_session_names(&previous, desired))?;

        let mut next = RuntimeState::new();
        next.socket_path = clean_path_str(&desired.socket_path.display().to_string());
        next.config_path = clean_path_str(&desired.config_path.display().to_string());
        let global_changed =
            previous.socket_path != next.socket_path || previous.config_path != next.config_path;

        let mut report = ReconcileReport::default();
        for name in sorted_desired_workspace_names(&desired.workspaces) {
            let entry = desired
                .workspaces
                .get(&name)
                .expect("desired workspace exists");
            let record = desired_workspace_state(entry);
            let prev_record = previous.workspaces.get(&name).cloned();
            let session = sessions.get(&name).cloned().unwrap_or_default();
            let matches = prev_record
                .as_ref()
                .is_some_and(|prev| workspace_state_matches(prev, &record))
                && !global_changed;

            if matches {
                ensure_artifacts(
                    &name,
                    &entry.workspace,
                    &self.socket_path,
                    Some(&self.config_path),
                    &self.ax_bin,
                )?;
                if opts.daemon_running && !session.exists {
                    self.manager.create(&name, &entry.workspace)?;
                    report.add_action(
                        "workspace",
                        &name,
                        "create",
                        "session was missing and has been started",
                    );
                }
                next.workspaces.insert(name, record);
                continue;
            }

            let action = if prev_record.is_some() {
                "restart"
            } else {
                "create"
            };
            if session.exists && !opts.allow_disruptive_changes {
                report.add_action(
                    "workspace",
                    &name,
                    &format!("blocked_{action}"),
                    "reconcile mode forbids disrupting an existing session",
                );
                if let Some(prev) = prev_record {
                    next.workspaces.insert(name, prev);
                }
                continue;
            }
            if session.exists {
                if let Some(reason) = disruption_block_reason(&session) {
                    report.add_action("workspace", &name, &format!("blocked_{action}"), reason);
                    if let Some(prev) = prev_record {
                        next.workspaces.insert(name, prev);
                    }
                    continue;
                }
            }

            let cleanup_dir = prev_record
                .as_ref()
                .map(|prev| prev.dir.as_str())
                .unwrap_or(record.dir.as_str());
            cleanup_workspace_state(&self.tmux, &name, cleanup_dir)?;
            ensure_artifacts(
                &name,
                &entry.workspace,
                &self.socket_path,
                Some(&self.config_path),
                &self.ax_bin,
            )?;
            if opts.daemon_running {
                self.manager.create(&name, &entry.workspace)?;
            }
            let details = if opts.daemon_running {
                "generated artifacts refreshed and session started"
            } else {
                "generated artifacts refreshed"
            };
            report.add_action("workspace", &name, action, details);
            next.workspaces.insert(name, record);
        }

        for name in sorted_workspace_state_names(&previous.workspaces) {
            if desired.workspaces.contains_key(&name) {
                continue;
            }
            let prev_record = previous
                .workspaces
                .get(&name)
                .expect("previous workspace state exists")
                .clone();
            let session = sessions.get(&name).cloned().unwrap_or_default();
            if session.exists && !opts.allow_disruptive_changes {
                report.add_action(
                    "workspace",
                    &name,
                    "blocked_remove",
                    "reconcile mode forbids disrupting an existing session",
                );
                next.workspaces.insert(name, prev_record);
                continue;
            }
            if session.exists {
                if let Some(reason) = disruption_block_reason(&session) {
                    report.add_action("workspace", &name, "blocked_remove", reason);
                    next.workspaces.insert(name, prev_record);
                    continue;
                }
            }
            cleanup_workspace_state(&self.tmux, &name, &prev_record.dir)?;
            report.add_action(
                "workspace",
                &name,
                "remove",
                "generated artifacts cleaned up",
            );
        }

        for name in sorted_desired_orchestrator_names(&desired.orchestrators) {
            let entry = desired
                .orchestrators
                .get(&name)
                .expect("desired orchestrator exists");
            let record = desired_orchestrator_state(entry);
            let prev_record = previous.orchestrators.get(&name).cloned();
            let session = sessions.get(&name).cloned().unwrap_or_default();
            let matches = prev_record
                .as_ref()
                .is_some_and(|prev| orchestrator_state_matches(prev, &record))
                && !global_changed;

            if entry.root {
                ensure_orchestrator(
                    &self.tmux,
                    &entry.node,
                    &entry.parent_name,
                    &self.socket_path,
                    Some(&self.config_path),
                    &self.ax_bin,
                    false,
                )?;
                if !matches {
                    report.require_root_manual_restart(
                        "root orchestrator artifacts changed; manual relaunch is required",
                    );
                    let operation = if prev_record.is_some() {
                        "manual_restart_required"
                    } else {
                        "create_artifacts"
                    };
                    let details = if prev_record.is_some() {
                        "root foreground orchestrator is not hot-reloaded"
                    } else {
                        "root orchestrator artifacts created"
                    };
                    report.add_action("orchestrator", &name, operation, details);
                }
                next.orchestrators.insert(name, record);
                continue;
            }

            if matches {
                ensure_orchestrator(
                    &self.tmux,
                    &entry.node,
                    &entry.parent_name,
                    &self.socket_path,
                    Some(&self.config_path),
                    &self.ax_bin,
                    opts.daemon_running,
                )?;
                if opts.daemon_running && !session.exists && entry.managed_session {
                    report.add_action(
                        "orchestrator",
                        &name,
                        "create",
                        "session was missing and has been started",
                    );
                }
                next.orchestrators.insert(name, record);
                continue;
            }

            let action = if prev_record.is_some() {
                "restart"
            } else {
                "create"
            };
            if session.exists && !opts.allow_disruptive_changes {
                report.add_action(
                    "orchestrator",
                    &name,
                    &format!("blocked_{action}"),
                    "reconcile mode forbids disrupting an existing session",
                );
                if let Some(prev) = prev_record {
                    next.orchestrators.insert(name, prev);
                }
                continue;
            }
            if session.exists {
                if let Some(reason) = disruption_block_reason(&session) {
                    report.add_action("orchestrator", &name, &format!("blocked_{action}"), reason);
                    if let Some(prev) = prev_record {
                        next.orchestrators.insert(name, prev);
                    }
                    continue;
                }
            }

            let cleanup_dir = prev_record
                .as_ref()
                .map(|prev| prev.artifact_dir.as_str())
                .unwrap_or(record.artifact_dir.as_str());
            cleanup_orchestrator_state(&self.tmux, &name, Path::new(cleanup_dir))?;
            ensure_orchestrator(
                &self.tmux,
                &entry.node,
                &entry.parent_name,
                &self.socket_path,
                Some(&self.config_path),
                &self.ax_bin,
                opts.daemon_running,
            )?;
            let details = if opts.daemon_running && entry.managed_session {
                "generated artifacts refreshed and session started"
            } else {
                "generated artifacts refreshed"
            };
            report.add_action("orchestrator", &name, action, details);
            next.orchestrators.insert(name, record);
        }

        for name in sorted_orchestrator_state_names(&previous.orchestrators) {
            if desired.orchestrators.contains_key(&name) {
                continue;
            }
            let prev_record = previous
                .orchestrators
                .get(&name)
                .expect("previous orchestrator state exists")
                .clone();
            let session = sessions.get(&name).cloned().unwrap_or_default();
            if session.exists && !opts.allow_disruptive_changes {
                report.add_action(
                    "orchestrator",
                    &name,
                    "blocked_remove",
                    "reconcile mode forbids disrupting an existing session",
                );
                next.orchestrators.insert(name, prev_record);
                continue;
            }
            if session.exists {
                if let Some(reason) = disruption_block_reason(&session) {
                    report.add_action("orchestrator", &name, "blocked_remove", reason);
                    next.orchestrators.insert(name, prev_record);
                    continue;
                }
            }
            cleanup_orchestrator_state(&self.tmux, &name, Path::new(&prev_record.artifact_dir))?;
            report.add_action(
                "orchestrator",
                &name,
                "remove",
                "generated artifacts cleaned up",
            );
        }

        save_runtime_state(&state_path, &next)?;
        Ok(report)
    }
}

#[must_use]
pub fn build_desired_state(
    config: &Config,
    socket_path: impl Into<PathBuf>,
    config_path: impl Into<PathBuf>,
) -> DesiredState {
    let socket_path = socket_path.into();
    let config_path = config_path.into();
    let mut workspaces = BTreeMap::new();
    for (name, workspace) in &config.workspaces {
        workspaces.insert(
            name.clone(),
            DesiredWorkspace {
                name: name.clone(),
                workspace: workspace.clone(),
            },
        );
    }
    DesiredState {
        socket_path,
        config_path,
        workspaces,
        orchestrators: BTreeMap::new(),
    }
}

pub fn build_desired_state_with_tree(
    config: &Config,
    tree: &ProjectNode,
    socket_path: impl Into<PathBuf>,
    config_path: impl Into<PathBuf>,
    include_root: bool,
) -> Result<DesiredState, ReconcileError> {
    let mut desired = build_desired_state(config, socket_path, config_path);
    append_desired_orchestrators(&mut desired, tree, "", include_root)?;
    Ok(desired)
}

fn append_desired_orchestrators(
    desired: &mut DesiredState,
    node: &ProjectNode,
    parent_name: &str,
    include_self: bool,
) -> Result<(), ReconcileError> {
    let self_name = orchestrator_name(&node.prefix);
    let runtime = Runtime::normalize(&node.orchestrator_runtime).ok_or_else(|| {
        OrchestratorError::InvalidRuntime {
            name: self_name.clone(),
            runtime: node.orchestrator_runtime.clone(),
        }
    })?;

    let mut child_parent_name = parent_name.to_owned();
    if include_self {
        let artifact_dir = orchestrator_dir_for_node(node)?;
        let prompt = orchestrator_prompt(node, &node.prefix, parent_name);
        desired.orchestrators.insert(
            self_name.clone(),
            DesiredOrchestrator {
                name: self_name.clone(),
                node: node.clone(),
                parent_name: parent_name.to_owned(),
                artifact_dir,
                runtime: runtime.as_str().to_owned(),
                root: node.prefix.is_empty(),
                managed_session: !node.prefix.is_empty(),
                prompt_hash: hash_text(&prompt),
            },
        );
        child_parent_name = self_name;
    }

    for child in &node.children {
        append_desired_orchestrators(desired, child, &child_parent_name, true)?;
    }
    Ok(())
}

impl ReconcileReport {
    fn add_action(&mut self, kind: &str, name: &str, operation: &str, details: &str) {
        self.actions.push(ReconcileAction {
            kind: kind.to_owned(),
            name: name.to_owned(),
            operation: operation.to_owned(),
            details: details.to_owned(),
        });
    }

    fn require_root_manual_restart(&mut self, reason: &str) {
        self.root_manual_restart_required = true;
        let trimmed = reason.trim();
        if trimmed.is_empty() {
            return;
        }
        if self
            .root_manual_restart_reasons
            .iter()
            .any(|existing| existing == trimmed)
        {
            return;
        }
        self.root_manual_restart_reasons.push(trimmed.to_owned());
    }
}

impl RuntimeState {
    fn new() -> Self {
        Self {
            version: RUNTIME_STATE_VERSION,
            socket_path: String::new(),
            config_path: String::new(),
            workspaces: BTreeMap::new(),
            orchestrators: BTreeMap::new(),
        }
    }
}

fn desired_workspace_state(entry: &DesiredWorkspace) -> WorkspaceState {
    WorkspaceState {
        name: entry.name.clone(),
        dir: clean_path_str(&entry.workspace.dir),
        runtime: clean_runtime(&entry.workspace.runtime),
        agent: entry.workspace.agent.trim().to_owned(),
        shell: entry.workspace.shell.trim().to_owned(),
        env: entry.workspace.env.clone(),
        instructions_hash: hash_text(entry.workspace.instructions.trim()),
    }
}

fn desired_orchestrator_state(entry: &DesiredOrchestrator) -> OrchestratorState {
    OrchestratorState {
        name: entry.name.clone(),
        artifact_dir: clean_path_str(&entry.artifact_dir.display().to_string()),
        runtime: entry.runtime.clone(),
        parent_name: entry.parent_name.clone(),
        prompt_hash: entry.prompt_hash.clone(),
        managed_session: entry.managed_session,
        root: entry.root,
    }
}

fn workspace_state_matches(a: &WorkspaceState, b: &WorkspaceState) -> bool {
    a.name == b.name
        && a.dir == b.dir
        && a.runtime == b.runtime
        && a.agent == b.agent
        && a.shell == b.shell
        && a.instructions_hash == b.instructions_hash
        && a.env == b.env
}

fn orchestrator_state_matches(a: &OrchestratorState, b: &OrchestratorState) -> bool {
    a.name == b.name
        && a.artifact_dir == b.artifact_dir
        && a.runtime == b.runtime
        && a.parent_name == b.parent_name
        && a.prompt_hash == b.prompt_hash
        && a.managed_session == b.managed_session
        && a.root == b.root
}

fn load_runtime_state(path: &Path) -> Result<RuntimeState, ReconcileError> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(RuntimeState::new()),
        Err(source) => {
            return Err(ReconcileError::ReadState {
                path: path.display().to_string(),
                source,
            });
        }
    };

    let mut state: RuntimeState =
        serde_json::from_slice(&data).map_err(|source| ReconcileError::ParseState {
            path: path.display().to_string(),
            source,
        })?;
    if state.workspaces.is_empty() {
        state.workspaces = BTreeMap::new();
    }
    if state.orchestrators.is_empty() {
        state.orchestrators = BTreeMap::new();
    }
    Ok(state)
}

fn save_runtime_state(path: &Path, state: &RuntimeState) -> Result<(), ReconcileError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ReconcileError::WriteState {
            path: path.display().to_string(),
            source,
        })?;
    }
    let mut body = serde_json::to_vec_pretty(state)?;
    body.push(b'\n');
    fs::write(path, body).map_err(|source| ReconcileError::WriteState {
        path: path.display().to_string(),
        source,
    })
}

fn reconcile_state_path(config_path: &Path) -> PathBuf {
    let base = config_path.parent().unwrap_or_else(|| Path::new("."));
    let base = if base.as_os_str().is_empty() {
        Path::new(".")
    } else {
        base
    };
    base.join(RUNTIME_STATE_FILE)
}

fn desired_session_names(previous: &RuntimeState, desired: &DesiredState) -> Vec<String> {
    let mut names = BTreeMap::new();
    for name in previous.workspaces.keys() {
        names.insert(name.clone(), ());
    }
    for name in previous.orchestrators.keys() {
        names.insert(name.clone(), ());
    }
    for name in desired.workspaces.keys() {
        names.insert(name.clone(), ());
    }
    for name in desired.orchestrators.keys() {
        names.insert(name.clone(), ());
    }
    names.into_keys().collect()
}

fn load_session_snapshots<B: TmuxBackend>(
    tmux: &B,
    names: Vec<String>,
) -> Result<BTreeMap<String, SessionSnapshot>, ReconcileError> {
    let mut result = BTreeMap::new();
    if names.is_empty() {
        return Ok(result);
    }

    let listed = tmux.list_sessions()?;
    let mut by_name = BTreeMap::new();
    for session in listed {
        by_name.insert(session.workspace.clone(), session);
    }

    for name in names {
        let Some(info) = by_name.get(&name) else {
            continue;
        };
        let idle = if info.attached {
            false
        } else {
            tmux.is_idle(&name)
        };
        result.insert(
            name,
            SessionSnapshot {
                exists: true,
                attached: info.attached,
                idle,
            },
        );
    }
    Ok(result)
}

fn disruption_block_reason(snapshot: &SessionSnapshot) -> Option<&'static str> {
    if snapshot.attached {
        return Some("tmux session is attached");
    }
    if !snapshot.idle {
        return Some("tmux session is not idle");
    }
    None
}

fn sorted_desired_workspace_names(entries: &BTreeMap<String, DesiredWorkspace>) -> Vec<String> {
    entries.keys().cloned().collect()
}

fn sorted_workspace_state_names(entries: &BTreeMap<String, WorkspaceState>) -> Vec<String> {
    entries.keys().cloned().collect()
}

fn sorted_desired_orchestrator_names(
    entries: &BTreeMap<String, DesiredOrchestrator>,
) -> Vec<String> {
    entries.keys().cloned().collect()
}

fn sorted_orchestrator_state_names(entries: &BTreeMap<String, OrchestratorState>) -> Vec<String> {
    entries.keys().cloned().collect()
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

fn clean_runtime(runtime: &str) -> String {
    match runtime.trim().to_ascii_lowercase().as_str() {
        "" | "claude" => "claude".to_owned(),
        "codex" => "codex".to_owned(),
        other => other.to_owned(),
    }
}

fn clean_path_str(path: &str) -> String {
    if path.trim().is_empty() {
        return String::new();
    }
    let cleaned = normalize_path(Path::new(path));
    cleaned.display().to_string()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let is_absolute = path.is_absolute();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !is_absolute {
                    out.push("..");
                }
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }

    if out.as_os_str().is_empty() {
        if is_absolute {
            PathBuf::from("/")
        } else {
            PathBuf::from(".")
        }
    } else {
        out
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::normalize_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn normalize_path_preserves_relative_dot() {
        assert_eq!(normalize_path(Path::new(".")), PathBuf::from("."));
        assert_eq!(normalize_path(Path::new("./a/../b")), PathBuf::from("b"));
    }
}
