//! Shared domain types referenced by request/response payloads:
//! Task, Message, Workspace info, team reconfigure state, ….
//!
//! Map fields use [`std::collections::BTreeMap`] so serialized JSON
//! keys come out alphabetically sorted — the wire format depends on
//! that ordering.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::helpers::{is_false, is_zero_i64};

/// Status reported by a workspace over the daemon socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    #[serde(rename = "online")]
    Online,
    #[serde(rename = "offline")]
    Offline,
    #[serde(rename = "disconnected")]
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub dir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_status: Option<WorkspaceGitStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_at: Option<DateTime<Utc>>,
    /// Wall-clock timestamp of the most recent daemon-visible activity
    /// from this workspace (register, status update, message send,
    /// tool call, etc.). Consumers use this to detect peers that are
    /// connected but not making progress — the "how long has B been
    /// quiet" signal that presence alone cannot answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Count of non-removed tasks assigned to this workspace that are
    /// still open (`Pending` / `InProgress` / `Blocked`). Lets an
    /// orchestrator answer "how loaded is B right now" without
    /// enumerating tasks per workspace.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub active_task_count: i64,
    /// ID of the task this workspace is currently driving, when one
    /// can be attributed — specifically the most recently updated
    /// `InProgress` task assigned to this workspace. `None` means
    /// the workspace is idle or has no attributable task in flight.
    /// An orchestrator can join this with `get_task` for a cheap
    /// "what is B working on right now" answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task_id: Option<String>,
    /// Monotonic generation number assigned by the daemon each time
    /// this workspace registers. A bump since the last snapshot means
    /// the peer disconnected and came back — useful for invalidating
    /// any caller-side assumptions (cached task state, in-flight
    /// requests, dropped messages) that were tied to the prior
    /// connection. `0` is treated as "unknown" for offline peers.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub connection_generation: u64,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceGitStatus {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub modified: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub added: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub deleted: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub untracked: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub files_changed: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub insertions: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub deletions: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpToolActivityStatus {
    #[default]
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "error")]
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleAction {
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "restart")]
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleTargetKind {
    #[serde(rename = "workspace")]
    Workspace,
    #[serde(rename = "orchestrator")]
    Orchestrator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleTarget {
    pub name: String,
    pub kind: LifecycleTargetKind,
    pub managed_session: bool,
}

// ---------- Team reconfigure ----------

pub const EXPERIMENTAL_MCP_TEAM_RECONFIGURE_FLAG_KEY: &str = "experimental_mcp_team_reconfigure";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeamEntryKind {
    #[serde(rename = "workspace")]
    Workspace,
    #[serde(rename = "child")]
    Child,
    #[serde(rename = "root_orchestrator")]
    RootOrchestrator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeamChangeOp {
    #[serde(rename = "add")]
    Add,
    #[serde(rename = "remove")]
    Remove,
    #[serde(rename = "enable")]
    Enable,
    #[serde(rename = "disable")]
    Disable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeamReconcileMode {
    #[serde(rename = "artifacts_only")]
    ArtifactsOnly,
    #[serde(rename = "start_missing")]
    StartMissing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamWorkspaceSpec {
    pub dir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shell: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runtime: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub codex_model_reasoning_effort: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamChildSpec {
    pub dir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamReconfigureChange {
    pub op: TeamChangeOp,
    pub kind: TeamEntryKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<TeamWorkspaceSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<TeamChildSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamOverlay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_root_orchestrator: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub added_workspaces: BTreeMap<String, TeamWorkspaceSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub removed_workspaces: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub disabled_workspaces: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub added_children: BTreeMap<String, TeamChildSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub removed_children: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub disabled_children: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfiguredState {
    pub root_orchestrator_enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orchestrators: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamReconfigureAction {
    pub action: String,
    pub kind: TeamEntryKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamApplyReport {
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub success: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconcile_mode: Option<TeamReconcileMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TeamReconfigureAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamReconfigureState {
    pub team_id: String,
    pub base_config_path: String,
    pub effective_config_path: String,
    pub feature_enabled: bool,
    pub revision: i64,
    #[serde(default, skip_serializing_if = "team_overlay_is_empty")]
    pub overlay: TeamOverlay,
    pub desired: TeamConfiguredState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_apply: Option<TeamApplyReport>,
}

fn team_overlay_is_empty(o: &TeamOverlay) -> bool {
    o.disable_root_orchestrator.is_none()
        && o.added_workspaces.is_empty()
        && o.removed_workspaces.is_empty()
        && o.disabled_workspaces.is_empty()
        && o.added_children.is_empty()
        && o.removed_children.is_empty()
        && o.disabled_children.is_empty()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamReconfigurePlan {
    pub state: TeamReconfigureState,
    pub expected_revision: i64,
    pub changes: Vec<TeamReconfigureChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TeamReconfigureAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamApplyTicket {
    pub token: String,
    pub plan: TeamReconfigurePlan,
    pub reconcile_mode: TeamReconcileMode,
}

// ---------- Task management ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStartMode {
    // Older persisted snapshots used an empty string to mean
    // "unset / use default". Accept both on the wire so legacy state
    // files still load.
    #[serde(rename = "default", alias = "")]
    Default,
    #[serde(rename = "fresh")]
    Fresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskWorkflowMode {
    #[serde(rename = "parallel", alias = "")]
    Parallel,
    #[serde(rename = "serial")]
    Serial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPriority {
    #[serde(rename = "low")]
    Low,
    // Empty string is the historical "unset" value; map it to
    // Normal for backwards compat.
    #[serde(rename = "normal", alias = "")]
    Normal,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "urgent")]
    Urgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskSequenceState {
    #[serde(rename = "waiting_turn")]
    WaitingTurn,
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "released")]
    Released,
    #[serde(rename = "not_applicable")]
    NotApplicable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub assignee: String,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_task_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_task_ids: Vec<String>,
    pub version: i64,
    pub status: TaskStatus,
    pub start_mode: TaskStartMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_mode: Option<TaskWorkflowMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<TaskPriority>,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub stale_after_seconds: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dispatch_message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dispatch_config_path: String,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub dispatch_count: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub attempt_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dispatch_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub claimed_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub claim_source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<TaskLog>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollup: Option<TaskRollup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<TaskSequenceInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_info: Option<TaskStaleInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub removed_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub remove_reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskLog {
    pub timestamp: DateTime<Utc>,
    pub workspace: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRollup {
    pub total_children: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub pending_children: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub in_progress_children: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub blocked_children: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub completed_children: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub failed_children: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub cancelled_children: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub terminal_children: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub active_children: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_child_update_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub all_children_terminal: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub needs_parent_reconciliation: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSequenceInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<TaskWorkflowMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskSequenceState>,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub position: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub waiting_on_task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // wire-compat DTO; flags are semantically independent
                                         // (wake / claim / recovery / divergence) and any regrouping would change the JSON layout.
pub struct TaskStaleInfo {
    pub is_stale: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recommended_action: String,
    pub last_progress_at: DateTime<Utc>,
    pub age_seconds: i64,
    pub pending_messages: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub wake_pending: bool,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub wake_attempts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_wake_retry_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub claim_state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub claim_state_note: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub runnable: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runnable_reason: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub recovery_eligible: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub state_divergence: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state_divergence_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub scope: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub superseded_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
