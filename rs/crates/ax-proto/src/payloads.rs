//! Request payload types. Mirrors `internal/daemon/protocol.go`.

use serde::{Deserialize, Serialize};

use crate::helpers::is_zero_i64;

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
    /// The effective config path the daemon should use when dispatching the
    /// target workspace. Empty string disables the dispatch side-effect.
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
