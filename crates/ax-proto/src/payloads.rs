//! Request payload types.

use serde::{Deserialize, Serialize};

use crate::helpers::{is_false, is_zero_i64};
use crate::types::{
    LifecycleAction, McpToolActivityStatus, TaskStatus, TeamReconcileMode, TeamReconfigureAction,
    TeamReconfigureChange,
};

/// Sent by a workspace process when it attaches to the daemon.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegisterPayload {
    pub workspace: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub config_path: String,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub idle_timeout_seconds: i64,
}

/// Point-to-point message from the caller's workspace to `to`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendMessagePayload {
    pub to: String,
    pub message: String,
    /// Effective config path the daemon uses when dispatching the target
    /// workspace. Empty string disables the dispatch side-effect.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub config_path: String,
}

/// Broadcast to every other registered workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BroadcastPayload {
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub config_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadMessagesPayload {
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub limit: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetStatusPayload {
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordMcpToolActivityPayload {
    pub tool: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    pub status: McpToolActivityStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error_kind: String,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlLifecyclePayload {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub config_path: String,
    pub name: String,
    pub action: LifecycleAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLifecyclePayload {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub config_path: String,
    pub name: String,
    pub action: LifecycleAction,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetSharedPayload {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetSharedPayload {
    pub key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RememberMemoryPayload {
    pub scope: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Serialises as `supersedes_ids` in the JSON wire format.
    #[serde(
        default,
        rename = "supersedes_ids",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub supersedes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallMemoriesPayload {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_superseded: bool,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub limit: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageTrendWorkspace {
    pub workspace: String,
    pub cwd: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageTrendsPayload {
    pub workspaces: Vec<UsageTrendWorkspace>,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub since_minutes: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub bucket_minutes: i64,
}

// ---------- Task payloads ----------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateTaskPayload {
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub assignee: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_task_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub start_mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workflow_mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub priority: String,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub stale_after_seconds: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StartTaskPayload {
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub message: String,
    pub assignee: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_task_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub start_mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workflow_mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub priority: String,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub stale_after_seconds: i64,
}

/// `UpdateTaskPayload`'s nullable fields intentionally preserve the
/// three-state semantics: absent means "don't change",
/// present-but-empty-string means "clear". serde `Option<T>` models that
/// exactly when paired with `skip_serializing_if = "Option::is_none"`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateTaskPayload {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Explicit self-verification affirmation. Required when
    /// transitioning the task to `Completed`. The daemon returns a
    /// `CompletionRequiresConfirmation` error otherwise, whose
    /// message spells out the checklist to re-read before the
    /// caller sets this to `true` and retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetTaskPayload {
    pub id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListTasksPayload {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub assignee: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CancelTaskPayload {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoveTaskPayload {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterveneTaskPayload {
    pub id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetTeamStatePayload {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub config_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamReconfigurePayload {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub config_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<TeamReconfigureChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconcile_mode: Option<TeamReconcileMode>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FinishTeamReconfigurePayload {
    pub token: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TeamReconfigureAction>,
}
