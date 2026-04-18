//! Response payload types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::{
    LifecycleAction, LifecycleTarget, Memory, Message, Task, TeamApplyTicket, TeamReconfigurePlan,
    TeamReconfigureState, WorkspaceInfo,
};
use crate::usage::WorkspaceTrend;

/// Canonical single-verb response (`register`, `set_status`, `set_shared`, …).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusResponse {
    pub status: String,
}

/// Response to `MsgSendMessage`. The `message_id` is present on successful
/// dispatch and empty when the message was suppressed as a no-op duplicate.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendMessageResponse {
    pub message_id: String,
    pub status: String,
}

/// Response to `MsgBroadcast`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BroadcastResponse {
    pub recipients: Vec<String>,
    pub count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadMessagesResponse {
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListWorkspacesResponse {
    pub workspaces: Vec<WorkspaceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlLifecycleResponse {
    pub target: LifecycleTarget,
    pub action: LifecycleAction,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // wire-compat DTO; each flag has independent meaning
pub struct AgentLifecycleResponse {
    pub name: String,
    pub action: String,
    pub target_kind: String,
    pub managed_session: bool,
    pub exact_match: bool,
    pub status: String,
    pub session_exists_before: bool,
    pub session_exists_after: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetSharedResponse {
    pub key: String,
    pub value: String,
    pub found: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListSharedResponse {
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResponse {
    pub memory: Memory,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallMemoriesResponse {
    pub memories: Vec<Memory>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageTrendsResponse {
    pub trends: Vec<WorkspaceTrend>,
}

// ---------- Task responses ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResponse {
    pub task: Task,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskDispatch {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartTaskResponse {
    pub task: Task,
    pub dispatch: TaskDispatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListTasksResponse {
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterveneTaskResponse {
    pub task: Task,
    pub action: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message_id: String,
}

// ---------- Team reconfigure responses ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamStateResponse {
    pub state: TeamReconfigureState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamPlanResponse {
    pub plan: TeamReconfigurePlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamApplyResponse {
    pub ticket: TeamApplyTicket,
}
