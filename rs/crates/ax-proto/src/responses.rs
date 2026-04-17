//! Response payload types. Mirrors `internal/daemon/protocol.go` response
//! structs.

use serde::{Deserialize, Serialize};

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
