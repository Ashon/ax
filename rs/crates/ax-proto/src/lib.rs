//! Wire protocol types for the ax daemon.
//!
//! This crate is the Rust port of `internal/daemon/protocol.go`. Every type
//! here is serde-compatible with the on-wire JSON encoding the Go daemon
//! produces today, so a Rust client can talk to a Go daemon (and eventually a
//! Rust daemon can accept connections from existing Go CLI / MCP server
//! binaries) during the migration.
//!
//! Go encoding conventions mapped to serde:
//! - `json:"foo"`           → `#[serde(rename = "foo")]` (or matching name)
//! - `json:"foo,omitempty"` → `#[serde(default, skip_serializing_if = "…")]`
//!
//! Go's `omitempty` skips zero values; we use field-type-specific predicates
//! (`String::is_empty`, `is_zero_i64`, `Option::is_none`, …) rather than a
//! single generic predicate, to keep the generated wire format stable.

#![forbid(unsafe_code)]

mod envelope;
pub mod helpers;
mod payloads;
mod responses;
pub mod types;

pub use envelope::{Envelope, ErrorPayload, MessageType, ResponsePayload};
pub use payloads::{BroadcastPayload, RegisterPayload, SendMessagePayload};
pub use responses::{BroadcastResponse, SendMessageResponse, StatusResponse};
