//! Wire protocol types for the ax daemon.
//!
//! Every type here is serde-compatible with the on-wire JSON encoding
//! used by the daemon, Unix-socket clients, the MCP server, and
//! persisted golden fixtures. `omitempty`-style skipping uses
//! field-type-specific predicates (`String::is_empty`, `is_zero_i64`,
//! `Option::is_none`, …) rather than a single generic predicate so the
//! generated wire format stays stable.

#![forbid(unsafe_code)]

mod envelope;
pub mod helpers;
pub mod payloads;
pub mod responses;
pub mod types;
pub mod usage;

pub use envelope::{Envelope, ErrorPayload, MessageType, ResponsePayload};
pub use payloads::{
    AgentLifecyclePayload, BroadcastPayload, CancelTaskPayload, ControlLifecyclePayload,
    CreateTaskPayload, FinishTeamReconfigurePayload, GetSharedPayload, GetTaskPayload,
    GetTeamStatePayload, InterveneTaskPayload, ListTasksPayload, ReadMessagesPayload,
    RecallMemoriesPayload, RegisterPayload, RememberMemoryPayload, RemoveTaskPayload,
    SendMessagePayload, SetSharedPayload, SetStatusPayload, StartTaskPayload,
    TeamReconfigurePayload, UpdateTaskPayload, UsageTrendWorkspace, UsageTrendsPayload,
};
pub use responses::{
    AgentLifecycleResponse, BroadcastResponse, ControlLifecycleResponse, GetSharedResponse,
    InterveneTaskResponse, ListSharedResponse, ListTasksResponse, ListWorkspacesResponse,
    MemoryResponse, ReadMessagesResponse, RecallMemoriesResponse, SendMessageResponse,
    StartTaskResponse, StatusResponse, TaskDispatch, TaskResponse, TeamApplyResponse,
    TeamPlanResponse, TeamStateResponse, UsageTrendsResponse,
};
