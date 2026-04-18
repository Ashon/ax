use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// Message types carried in the `type` field of an [`Envelope`].
///
/// String values must match the constants in `internal/daemon/protocol.go`
/// verbatim; any drift breaks Go ↔ Rust wire compatibility during the
/// migration. The variants are listed in the same order as the Go file for
/// easier side-by-side review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    #[serde(rename = "register")]
    Register,
    #[serde(rename = "unregister")]
    Unregister,
    #[serde(rename = "send_message")]
    SendMessage,
    #[serde(rename = "broadcast")]
    Broadcast,
    #[serde(rename = "read_messages")]
    ReadMessages,
    #[serde(rename = "list_workspaces")]
    ListWorkspaces,
    #[serde(rename = "set_status")]
    SetStatus,
    #[serde(rename = "control_lifecycle")]
    ControlLifecycle,
    #[serde(rename = "agent_lifecycle")]
    AgentLifecycle,
    #[serde(rename = "set_shared")]
    SetShared,
    #[serde(rename = "get_shared")]
    GetShared,
    #[serde(rename = "list_shared")]
    ListShared,
    #[serde(rename = "remember_memory")]
    RememberMemory,
    #[serde(rename = "recall_memories")]
    RecallMemories,
    #[serde(rename = "usage_trends")]
    UsageTrends,
    #[serde(rename = "create_task")]
    CreateTask,
    #[serde(rename = "start_task")]
    StartTask,
    #[serde(rename = "update_task")]
    UpdateTask,
    #[serde(rename = "get_task")]
    GetTask,
    #[serde(rename = "list_tasks")]
    ListTasks,
    #[serde(rename = "cancel_task")]
    CancelTask,
    #[serde(rename = "remove_task")]
    RemoveTask,
    #[serde(rename = "intervene_task")]
    InterveneTask,
    #[serde(rename = "get_team_state")]
    GetTeamState,
    #[serde(rename = "dry_run_team_reconfigure")]
    DryRunTeam,
    #[serde(rename = "apply_team_reconfigure")]
    ApplyTeam,
    #[serde(rename = "finish_team_reconfigure")]
    FinishTeam,
    #[serde(rename = "push_message")]
    PushMessage,
    #[serde(rename = "response")]
    Response,
    #[serde(rename = "error")]
    Error,
}

/// The newline-delimited JSON envelope exchanged over the Unix socket.
///
/// The payload is held as a [`RawValue`] so envelopes can be forwarded,
/// logged, or routed without paying the cost of decoding the inner type. Use
/// [`Envelope::decode_payload`] to materialise a typed payload when needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    #[serde(rename = "type")]
    pub r#type: MessageType,
    pub payload: Box<RawValue>,
}

impl Envelope {
    /// Build an envelope with `payload` serialised from any `Serialize` type.
    pub fn new<T: Serialize>(
        id: impl Into<String>,
        r#type: MessageType,
        payload: &T,
    ) -> Result<Self, serde_json::Error> {
        let data = serde_json::to_string(payload)?;
        let raw = RawValue::from_string(data)?;
        Ok(Self {
            id: id.into(),
            r#type,
            payload: raw,
        })
    }

    /// Decode the envelope's payload into a concrete type.
    pub fn decode_payload<'de, T: Deserialize<'de>>(&'de self) -> Result<T, serde_json::Error> {
        serde_json::from_str(self.payload.get())
    }
}

/// Wrapper the daemon writes for every successful response envelope. The
/// raw data blob matches whatever the handler returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsePayload {
    pub success: bool,
    pub data: Box<RawValue>,
}

/// Wrapper the daemon writes for error envelopes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub message: String,
}
