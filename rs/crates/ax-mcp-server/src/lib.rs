//! ax MCP server — thin translator between MCP tool invocations and
//! the ax daemon's Unix-socket envelope protocol. Mirrors the Go
//! `internal/mcpserver` package. The first slice ports the daemon
//! client so tool handlers have a stable async surface to call;
//! individual tool registrations land in follow-up slices.

#![forbid(unsafe_code)]

mod daemon_client;

pub use daemon_client::{
    DaemonClient, DaemonClientBuilder, DaemonClientError, DEFAULT_REQUEST_TIMEOUT,
};
