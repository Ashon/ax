//! Team-reconfigure controller. Mirrors
//! `internal/daemon/team_reconfigure.go` — the state machine that
//! powers the `get_team_state` / `dry_run_team_reconfigure` /
//! `apply_team_reconfigure` / `finish_team_reconfigure` handlers.
//!
//! Responsibilities:
//!   - Canonical team ID derivation from the base config path.
//!   - Overlay merge (add/remove/enable/disable for workspaces and
//!     children, plus the root-orchestrator toggle).
//!   - Materializing the effective YAML under `<state>/managed-teams/`.
//!   - Action diff between the current and next Config/ProjectNode.
//!   - Apply leases with a 2-minute TTL so parallel clients cannot
//!     race on the same team.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sha1::{Digest, Sha1};

use ax_config::{Child, Config, ConfigRoot, ProjectNode, Workspace};
use ax_proto::types::{
    TeamApplyReport, TeamApplyTicket, TeamChangeOp, TeamConfiguredState, TeamEntryKind,
    TeamOverlay, TeamReconcileMode, TeamReconfigureAction, TeamReconfigureChange,
    TeamReconfigurePlan, TeamReconfigureState, EXPERIMENTAL_MCP_TEAM_RECONFIGURE_FLAG_KEY,
};

use crate::shared_values::SharedValues;

const TEAM_APPLY_LEASE_TTL: Duration = Duration::from_secs(120);
const MANAGED_TEAMS_DIR: &str = "managed-teams";

#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error("config_path is required")]
    ConfigPathRequired,
    #[error("resolve config path {path:?}: {source}")]
    ResolveConfigPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("team revision mismatch: expected {expected}, got {got}")]
    RevisionMismatch { expected: i64, got: i64 },
    #[error("feature flag {0:?} is disabled")]
    FeatureDisabled(&'static str),
    #[error("invalid reconcile mode {0:?}")]
    InvalidReconcileMode(String),
    #[error("team reconfiguration already in progress for {0}")]
    ApplyInProgress(String),
    #[error("team reconfigure token {0:?} not found")]
    TokenNotFound(String),
    #[error("team state {0:?} not found")]
    StateNotFound(String),
    #[error("workspace change requires name")]
    WorkspaceNameRequired,
    #[error("workspace add requires workspace spec")]
    WorkspaceSpecRequired,
    #[error("workspace {0:?} already exists")]
    WorkspaceAlreadyExists(String),
    #[error("unsupported workspace op {0:?}")]
    UnsupportedWorkspaceOp(TeamChangeOp),
    #[error("child change requires name")]
    ChildNameRequired,
    #[error("child add requires child spec")]
    ChildSpecRequired,
    #[error("child {0:?} already exists")]
    ChildAlreadyExists(String),
    #[error("child add requires dir")]
    ChildDirRequired,
    #[error("unsupported child op {0:?}")]
    UnsupportedChildOp(TeamChangeOp),
    #[error("root orchestrator supports enable/disable only")]
    RootOrchestratorOpUnsupported,
    #[error("unsupported change kind {0:?}")]
    UnsupportedKind(TeamEntryKind),
    #[error("load config {path:?}: {source}")]
    LoadConfig {
        path: PathBuf,
        #[source]
        source: ax_config::TreeError,
    },
    #[error("read raw config {path:?}: {source}")]
    ReadRaw {
        path: PathBuf,
        #[source]
        source: ax_config::LoadError,
    },
    #[error("save effective config {path:?}: {source}")]
    Save {
        path: PathBuf,
        #[source]
        source: ax_config::LoadError,
    },
    #[error("create managed-teams dir {path:?}: {source}")]
    MkState {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("persist team state: {0}")]
    Persist(#[from] crate::team_state_store::TeamStateError),
}

/// Public entry point for the daemon. Holds the state store, the
/// managed-teams directory, and active apply leases.
#[derive(Debug)]
pub struct TeamController {
    state_dir: PathBuf,
    store: Arc<crate::team_state_store::TeamStateStore>,
    shared: Arc<SharedValues>,
    leases: Mutex<BTreeMap<String, TeamApplyLease>>,
}

#[derive(Debug, Clone)]
struct TeamApplyLease {
    team_id: String,
    token: String,
    started_at: DateTime<Utc>,
    expiry: DateTime<Utc>,
    reconcile_mode: TeamReconcileMode,
}

impl TeamController {
    #[must_use]
    pub fn new(
        state_dir: PathBuf,
        store: Arc<crate::team_state_store::TeamStateStore>,
        shared: Arc<SharedValues>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state_dir,
            store,
            shared,
            leases: Mutex::new(BTreeMap::new()),
        })
    }

    /// True when the experimental feature flag is set in the shared
    /// values store. Write handlers bail with [`TeamError::FeatureDisabled`]
    /// when this returns false; read handlers still return the current
    /// state with `feature_enabled = false` so clients can surface the
    /// toggle in UI.
    pub fn feature_enabled(&self) -> bool {
        self.shared
            .get(EXPERIMENTAL_MCP_TEAM_RECONFIGURE_FLAG_KEY)
            .is_some_and(|v| parse_bool_flag(&v))
    }

    pub fn get_state(&self, config_path: &str) -> Result<TeamReconfigureState, TeamError> {
        let base = canonical_config_path(config_path)?;
        self.current_state(&base, self.feature_enabled())
    }

    pub fn plan(
        &self,
        config_path: &str,
        expected_revision: Option<i64>,
        changes: &[TeamReconfigureChange],
    ) -> Result<TeamReconfigurePlan, TeamError> {
        let feature = self.feature_enabled();
        if !feature {
            return Err(TeamError::FeatureDisabled(
                EXPERIMENTAL_MCP_TEAM_RECONFIGURE_FLAG_KEY,
            ));
        }
        self.plan_inner(config_path, expected_revision, changes, feature)
    }

    pub fn begin_apply(
        &self,
        config_path: &str,
        expected_revision: Option<i64>,
        changes: &[TeamReconfigureChange],
        mode: Option<TeamReconcileMode>,
    ) -> Result<TeamApplyTicket, TeamError> {
        let feature = self.feature_enabled();
        if !feature {
            return Err(TeamError::FeatureDisabled(
                EXPERIMENTAL_MCP_TEAM_RECONFIGURE_FLAG_KEY,
            ));
        }
        let mode = mode.unwrap_or(TeamReconcileMode::ArtifactsOnly);
        let plan = self.plan_inner(config_path, expected_revision, changes, feature)?;

        let mut leases = self.leases.lock().expect("team controller leases poisoned");
        let now = Utc::now();
        if let Some(active) = leases.get(&plan.state.team_id) {
            if active.expiry > now {
                return Err(TeamError::ApplyInProgress(plan.state.team_id.clone()));
            }
        }

        let base_config_path = PathBuf::from(&plan.state.base_config_path);
        let mut persisted_state = plan.state.clone();
        let (effective_path, desired, _, _) =
            self.materialize_state(&base_config_path, &persisted_state, true)?;
        persisted_state.effective_config_path = effective_path;
        persisted_state.desired = desired;
        persisted_state.feature_enabled = feature;
        self.store.put(persisted_state.clone())?;

        let token = ticket_token(&persisted_state.team_id, now);
        leases.insert(
            persisted_state.team_id.clone(),
            TeamApplyLease {
                team_id: persisted_state.team_id.clone(),
                token: token.clone(),
                started_at: now,
                expiry: now + chrono::Duration::from_std(TEAM_APPLY_LEASE_TTL).expect("lease ttl"),
                reconcile_mode: mode.clone(),
            },
        );

        let mut plan_with_persisted = plan;
        plan_with_persisted.state = persisted_state;
        Ok(TeamApplyTicket {
            token,
            plan: plan_with_persisted,
            reconcile_mode: mode,
        })
    }

    pub fn finish_apply(
        &self,
        token: &str,
        success: bool,
        err_text: &str,
        actions: &[TeamReconfigureAction],
    ) -> Result<TeamReconfigureState, TeamError> {
        let feature = self.feature_enabled();
        let mut leases = self.leases.lock().expect("team controller leases poisoned");
        let now = Utc::now();
        let target_id = leases
            .iter()
            .find(|(_, active)| active.token == token)
            .map(|(id, _)| id.clone());
        let Some(team_id) = target_id else {
            return Err(TeamError::TokenNotFound(token.to_owned()));
        };
        let lease = leases.remove(&team_id).expect("lease existed");
        drop(leases);

        let mut state = self
            .store
            .get(&lease.team_id)
            .ok_or_else(|| TeamError::StateNotFound(lease.team_id.clone()))?;
        state.feature_enabled = feature;
        state.last_apply = Some(TeamApplyReport {
            started_at: lease.started_at,
            finished_at: Some(now),
            success,
            error: err_text.trim().to_owned(),
            reconcile_mode: Some(lease.reconcile_mode.clone()),
            actions: actions.to_vec(),
        });
        self.store.put(state.clone())?;
        Ok(state)
    }

    // ---------- inner helpers ----------

    fn plan_inner(
        &self,
        config_path: &str,
        expected_revision: Option<i64>,
        changes: &[TeamReconfigureChange],
        feature_enabled: bool,
    ) -> Result<TeamReconfigurePlan, TeamError> {
        let base = canonical_config_path(config_path)?;
        let current_state = self.current_state(&base, feature_enabled)?;
        if let Some(want) = expected_revision {
            if current_state.revision != want {
                return Err(TeamError::RevisionMismatch {
                    expected: want,
                    got: current_state.revision,
                });
            }
        }
        let current_cfg_path = if current_state.effective_config_path.trim().is_empty() {
            PathBuf::from(&current_state.base_config_path)
        } else {
            PathBuf::from(&current_state.effective_config_path)
        };
        let current_cfg = load_config(&current_cfg_path)?;
        let current_tree = load_tree(&current_cfg_path)?;

        let (next_overlay, warnings) = apply_changes(&base, &current_state.overlay, changes)?;
        let mut next_state = current_state.clone();
        next_state.revision = current_state.revision + 1;
        next_state.overlay = next_overlay;
        let (effective_path, desired, next_cfg, next_tree) =
            self.materialize_state(&base, &next_state, false)?;
        next_state.effective_config_path = effective_path;
        next_state.desired = desired;
        next_state.feature_enabled = feature_enabled;

        Ok(TeamReconfigurePlan {
            state: next_state,
            expected_revision: current_state.revision,
            changes: changes.to_vec(),
            actions: diff_team_actions(&current_cfg, &current_tree, &next_cfg, &next_tree),
            warnings,
        })
    }

    fn current_state(
        &self,
        base: &Path,
        feature_enabled: bool,
    ) -> Result<TeamReconfigureState, TeamError> {
        let team_id = base.display().to_string();
        if let Some(mut state) = self.store.get(&team_id) {
            state.feature_enabled = feature_enabled;
            if state.base_config_path.trim().is_empty() {
                state.base_config_path.clone_from(&team_id);
            }
            if state.effective_config_path.trim().is_empty() {
                state.effective_config_path.clone_from(&team_id);
            }
            return Ok(state);
        }
        let desired = summarize_current_desired(base)?;
        Ok(TeamReconfigureState {
            team_id: team_id.clone(),
            base_config_path: team_id.clone(),
            effective_config_path: team_id,
            feature_enabled,
            revision: 0,
            overlay: TeamOverlay::default(),
            desired,
            last_apply: None,
        })
    }

    fn materialize_state(
        &self,
        base: &Path,
        state: &TeamReconfigureState,
        persist: bool,
    ) -> Result<(String, TeamConfiguredState, Config, ProjectNode), TeamError> {
        if !overlay_has_changes(&state.overlay) {
            let cfg = load_config(base)?;
            let tree = load_tree(base)?;
            let raw = load_raw_config(base)?;
            let desired = build_desired_summary(&raw, &cfg, &tree);
            return Ok((base.display().to_string(), desired, cfg, tree));
        }

        let raw_cfg = materialize_raw_config(base, &state.overlay)?;
        let path = self.effective_config_path(base, persist)?;
        raw_cfg.save(&path).map_err(|source| TeamError::Save {
            path: path.clone(),
            source,
        })?;
        let cfg = load_config(&path)?;
        let tree = load_tree(&path)?;
        let desired = build_desired_summary(&raw_cfg, &cfg, &tree);
        Ok((path.display().to_string(), desired, cfg, tree))
    }

    fn effective_config_path(&self, base: &Path, persist: bool) -> Result<PathBuf, TeamError> {
        let dir = self.state_dir.join(MANAGED_TEAMS_DIR);
        std::fs::create_dir_all(&dir).map_err(|source| TeamError::MkState {
            path: dir.clone(),
            source,
        })?;
        let hash = short_team_hash(&base.display().to_string());
        if persist {
            Ok(dir.join(format!("{hash}.yaml")))
        } else {
            let ts = Utc::now().timestamp_nanos_opt().unwrap_or(0);
            Ok(dir.join(format!("{hash}-plan-{ts}.yaml")))
        }
    }
}

// ---------- overlay apply ----------

fn apply_changes(
    base: &Path,
    current_overlay: &TeamOverlay,
    changes: &[TeamReconfigureChange],
) -> Result<(TeamOverlay, Vec<String>), TeamError> {
    let mut overlay = current_overlay.clone();
    let mut current_raw = materialize_raw_config(base, &overlay)?;
    let mut warnings = Vec::new();
    for change in changes {
        apply_single_change(base, &current_raw, &mut overlay, &mut warnings, change)?;
        current_raw = materialize_raw_config(base, &overlay)?;
    }
    Ok((normalize_overlay(overlay), warnings))
}

fn apply_single_change(
    base: &Path,
    current_raw: &Config,
    overlay: &mut TeamOverlay,
    warnings: &mut Vec<String>,
    change: &TeamReconfigureChange,
) -> Result<(), TeamError> {
    match change.kind {
        TeamEntryKind::Workspace => {
            apply_workspace_change(base, current_raw, overlay, warnings, change)
        }
        TeamEntryKind::Child => apply_child_change(base, current_raw, overlay, warnings, change),
        TeamEntryKind::RootOrchestrator => {
            apply_root_orchestrator_change(overlay, warnings, change)
        }
    }
}

fn apply_workspace_change(
    base: &Path,
    current_raw: &Config,
    overlay: &mut TeamOverlay,
    warnings: &mut Vec<String>,
    change: &TeamReconfigureChange,
) -> Result<(), TeamError> {
    let name = change.name.trim().to_owned();
    if name.is_empty() {
        return Err(TeamError::WorkspaceNameRequired);
    }
    let exists = current_raw.workspaces.contains_key(&name);
    match change.op {
        TeamChangeOp::Add => {
            let Some(spec) = change.workspace.as_ref() else {
                return Err(TeamError::WorkspaceSpecRequired);
            };
            if exists {
                return Err(TeamError::WorkspaceAlreadyExists(name));
            }
            let root = config_root_dir(base);
            let mut spec = spec.clone();
            spec.dir = resolve_overlay_dir(&root, &spec.dir);
            overlay.added_workspaces.insert(name.clone(), spec);
            overlay.removed_workspaces.remove(&name);
            overlay.disabled_workspaces.remove(&name);
        }
        TeamChangeOp::Remove => {
            if !exists {
                warnings.push(format!("workspace {name:?} is already absent"));
                return Ok(());
            }
            overlay.added_workspaces.remove(&name);
            overlay.removed_workspaces.insert(name.clone(), true);
            overlay.disabled_workspaces.remove(&name);
        }
        TeamChangeOp::Disable => {
            if !exists {
                warnings.push(format!("workspace {name:?} is already absent/disabled"));
                return Ok(());
            }
            overlay.disabled_workspaces.insert(name.clone(), true);
            overlay.removed_workspaces.remove(&name);
        }
        TeamChangeOp::Enable => {
            if overlay.removed_workspaces.remove(&name).is_some() {
                return Ok(());
            }
            if overlay.disabled_workspaces.remove(&name).is_some() {
                return Ok(());
            }
            warnings.push(format!("workspace {name:?} is already enabled"));
        }
    }
    Ok(())
}

fn apply_child_change(
    base: &Path,
    current_raw: &Config,
    overlay: &mut TeamOverlay,
    warnings: &mut Vec<String>,
    change: &TeamReconfigureChange,
) -> Result<(), TeamError> {
    let name = change.name.trim().to_owned();
    if name.is_empty() {
        return Err(TeamError::ChildNameRequired);
    }
    let exists = current_raw.children.contains_key(&name);
    match change.op {
        TeamChangeOp::Add => {
            let Some(spec) = change.child.as_ref() else {
                return Err(TeamError::ChildSpecRequired);
            };
            if exists {
                return Err(TeamError::ChildAlreadyExists(name));
            }
            let root = config_root_dir(base);
            let mut spec = spec.clone();
            spec.dir = resolve_overlay_dir(&root, &spec.dir);
            if spec.dir.trim().is_empty() {
                return Err(TeamError::ChildDirRequired);
            }
            overlay.added_children.insert(name.clone(), spec);
            overlay.removed_children.remove(&name);
            overlay.disabled_children.remove(&name);
        }
        TeamChangeOp::Remove => {
            if !exists {
                warnings.push(format!("child {name:?} is already absent"));
                return Ok(());
            }
            overlay.added_children.remove(&name);
            overlay.removed_children.insert(name.clone(), true);
            overlay.disabled_children.remove(&name);
        }
        TeamChangeOp::Disable => {
            if !exists {
                warnings.push(format!("child {name:?} is already absent/disabled"));
                return Ok(());
            }
            overlay.disabled_children.insert(name.clone(), true);
            overlay.removed_children.remove(&name);
        }
        TeamChangeOp::Enable => {
            if overlay.removed_children.remove(&name).is_some() {
                return Ok(());
            }
            if overlay.disabled_children.remove(&name).is_some() {
                return Ok(());
            }
            warnings.push(format!("child {name:?} is already enabled"));
        }
    }
    Ok(())
}

fn apply_root_orchestrator_change(
    overlay: &mut TeamOverlay,
    _warnings: &mut [String],
    change: &TeamReconfigureChange,
) -> Result<(), TeamError> {
    match change.op {
        TeamChangeOp::Disable => overlay.disable_root_orchestrator = Some(true),
        TeamChangeOp::Enable => overlay.disable_root_orchestrator = Some(false),
        _ => return Err(TeamError::RootOrchestratorOpUnsupported),
    }
    Ok(())
}

// ---------- raw config materialization ----------

fn materialize_raw_config(base: &Path, overlay: &TeamOverlay) -> Result<Config, TeamError> {
    let mut raw = load_raw_config(base)?;
    let root = config_root_dir(base);

    for ws in raw.workspaces.values_mut() {
        ws.dir = resolve_overlay_dir(&root, &ws.dir);
    }
    for child in raw.children.values_mut() {
        child.dir = resolve_overlay_dir(&root, &child.dir);
    }

    if let Some(flag) = overlay.disable_root_orchestrator {
        raw.disable_root_orchestrator = flag;
    }
    for name in overlay.removed_workspaces.keys() {
        raw.workspaces.remove(name);
    }
    for name in overlay.disabled_workspaces.keys() {
        raw.workspaces.remove(name);
    }
    for (name, spec) in &overlay.added_workspaces {
        raw.workspaces.insert(
            name.clone(),
            Workspace {
                dir: resolve_overlay_dir(&root, &spec.dir),
                description: spec.description.clone(),
                shell: spec.shell.clone(),
                runtime: spec.runtime.clone(),
                codex_model_reasoning_effort: spec.codex_model_reasoning_effort.clone(),
                agent: spec.agent.clone(),
                instructions: spec.instructions.clone(),
                env: spec.env.clone(),
            },
        );
    }
    for name in overlay.removed_children.keys() {
        raw.children.remove(name);
    }
    for name in overlay.disabled_children.keys() {
        raw.children.remove(name);
    }
    for (name, spec) in &overlay.added_children {
        raw.children.insert(
            name.clone(),
            Child {
                dir: resolve_overlay_dir(&root, &spec.dir),
                prefix: spec.prefix.clone(),
            },
        );
    }
    Ok(raw)
}

// ---------- desired summary ----------

fn summarize_current_desired(base: &Path) -> Result<TeamConfiguredState, TeamError> {
    let cfg = load_config(base)?;
    let tree = load_tree(base)?;
    let raw = load_raw_config(base)?;
    Ok(build_desired_summary(&raw, &cfg, &tree))
}

fn build_desired_summary(raw: &Config, loaded: &Config, tree: &ProjectNode) -> TeamConfiguredState {
    let mut summary = TeamConfiguredState {
        root_orchestrator_enabled: !tree.disable_root_orchestrator,
        workspaces: loaded.workspaces.keys().cloned().collect(),
        children: raw.children.keys().cloned().collect(),
        orchestrators: Vec::new(),
    };
    let mut orchs: Vec<String> = collect_orchestrator_info(tree).into_keys().collect();
    orchs.sort();
    summary.orchestrators = orchs;
    summary
}

// ---------- action diff ----------

fn diff_team_actions(
    current_cfg: &Config,
    current_tree: &ProjectNode,
    next_cfg: &Config,
    next_tree: &ProjectNode,
) -> Vec<TeamReconfigureAction> {
    let mut actions = Vec::new();
    for (name, ws) in &current_cfg.workspaces {
        if !next_cfg.workspaces.contains_key(name) {
            actions.push(TeamReconfigureAction {
                action: "destroy".to_owned(),
                kind: TeamEntryKind::Workspace,
                name: name.clone(),
                dir: ws.dir.clone(),
                detail: String::new(),
            });
        }
    }
    for (name, ws) in &next_cfg.workspaces {
        if !current_cfg.workspaces.contains_key(name) {
            actions.push(TeamReconfigureAction {
                action: "ensure".to_owned(),
                kind: TeamEntryKind::Workspace,
                name: name.clone(),
                dir: ws.dir.clone(),
                detail: String::new(),
            });
        }
    }

    let current_orchs = collect_orchestrator_info(current_tree);
    let next_orchs = collect_orchestrator_info(next_tree);
    for (name, info) in &current_orchs {
        if !next_orchs.contains_key(name) {
            actions.push(TeamReconfigureAction {
                action: "destroy".to_owned(),
                kind: TeamEntryKind::RootOrchestrator,
                name: name.clone(),
                dir: info.dir.clone(),
                detail: String::new(),
            });
        }
    }
    for (name, info) in &next_orchs {
        if !current_orchs.contains_key(name) {
            actions.push(TeamReconfigureAction {
                action: "ensure".to_owned(),
                kind: TeamEntryKind::RootOrchestrator,
                name: name.clone(),
                dir: info.dir.clone(),
                detail: String::new(),
            });
        }
    }

    actions.sort_by(|a, b| {
        let kind_cmp = kind_rank(&a.kind).cmp(&kind_rank(&b.kind));
        if kind_cmp != std::cmp::Ordering::Equal {
            return kind_cmp;
        }
        a.name.cmp(&b.name).then_with(|| a.action.cmp(&b.action))
    });
    actions
}

fn kind_rank(kind: &TeamEntryKind) -> u8 {
    // Matches Go's lexicographic sort on the rendered string, so the
    // daemon emits `child < root_orchestrator < workspace`.
    match kind {
        TeamEntryKind::Child => 0,
        TeamEntryKind::RootOrchestrator => 1,
        TeamEntryKind::Workspace => 2,
    }
}

#[derive(Debug)]
struct OrchestratorInfo {
    dir: String,
}

fn collect_orchestrator_info(tree: &ProjectNode) -> BTreeMap<String, OrchestratorInfo> {
    let mut out = BTreeMap::new();
    walk_tree(tree, &mut out);
    out
}

fn walk_tree(node: &ProjectNode, out: &mut BTreeMap<String, OrchestratorInfo>) {
    if !(node.prefix.is_empty() && node.disable_root_orchestrator) {
        if let Ok(dir) = orchestrator_dir_for_node(node) {
            let name = if node.prefix.is_empty() {
                "orchestrator".to_owned()
            } else {
                format!("{}.orchestrator", node.prefix)
            };
            out.insert(name, OrchestratorInfo { dir });
        }
    }
    for child in &node.children {
        walk_tree(child, out);
    }
}

fn orchestrator_dir_for_node(node: &ProjectNode) -> std::io::Result<String> {
    if node.prefix.is_empty() {
        let home = std::env::var_os("HOME").ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "HOME env var not set")
        })?;
        return Ok(PathBuf::from(home)
            .join(".ax")
            .join("orchestrator")
            .display()
            .to_string());
    }
    let safe = node.prefix.replace('.', "_");
    Ok(node
        .dir
        .join(".ax")
        .join(format!("orchestrator-{safe}"))
        .display()
        .to_string())
}

// ---------- helpers ----------

fn canonical_config_path(config_path: &str) -> Result<PathBuf, TeamError> {
    let trimmed = config_path.trim();
    if trimmed.is_empty() {
        return Err(TeamError::ConfigPathRequired);
    }
    let path = PathBuf::from(trimmed);
    std::path::absolute(&path).map_err(|source| TeamError::ResolveConfigPath { path, source })
}

fn load_config(path: &Path) -> Result<Config, TeamError> {
    Config::load(path).map_err(|source| TeamError::LoadConfig {
        path: path.to_path_buf(),
        source,
    })
}

fn load_tree(path: &Path) -> Result<ProjectNode, TeamError> {
    Config::load_tree(path).map_err(|source| TeamError::LoadConfig {
        path: path.to_path_buf(),
        source,
    })
}

fn load_raw_config(path: &Path) -> Result<Config, TeamError> {
    Config::read_local(path).map_err(|source| TeamError::ReadRaw {
        path: path.to_path_buf(),
        source,
    })
}

fn config_root_dir(config_path: &Path) -> PathBuf {
    ConfigRoot::from_config_path(config_path).0
}

fn resolve_overlay_dir(base_dir: &Path, value: &str) -> String {
    let mut v = value.trim();
    if v.is_empty() {
        v = ".";
    }
    if let Some(rest) = v.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest).display().to_string();
        }
    }
    let path = Path::new(v);
    if path.is_absolute() {
        return path.display().to_string();
    }
    base_dir.join(path).display().to_string()
}

fn overlay_has_changes(overlay: &TeamOverlay) -> bool {
    overlay.disable_root_orchestrator.is_some()
        || !overlay.added_workspaces.is_empty()
        || !overlay.removed_workspaces.is_empty()
        || !overlay.disabled_workspaces.is_empty()
        || !overlay.added_children.is_empty()
        || !overlay.removed_children.is_empty()
        || !overlay.disabled_children.is_empty()
}

fn normalize_overlay(mut overlay: TeamOverlay) -> TeamOverlay {
    if overlay.added_workspaces.is_empty() {
        overlay.added_workspaces = BTreeMap::new();
    }
    if overlay.removed_workspaces.is_empty() {
        overlay.removed_workspaces = BTreeMap::new();
    }
    if overlay.disabled_workspaces.is_empty() {
        overlay.disabled_workspaces = BTreeMap::new();
    }
    if overlay.added_children.is_empty() {
        overlay.added_children = BTreeMap::new();
    }
    if overlay.removed_children.is_empty() {
        overlay.removed_children = BTreeMap::new();
    }
    if overlay.disabled_children.is_empty() {
        overlay.disabled_children = BTreeMap::new();
    }
    overlay
}

fn short_team_hash(value: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..6])
}

fn ticket_token(team_id: &str, now: DateTime<Utc>) -> String {
    format!(
        "{}-{}",
        short_team_hash(team_id),
        now.timestamp_nanos_opt().unwrap_or(0)
    )
}

fn parse_bool_flag(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "enabled"
    )
}
