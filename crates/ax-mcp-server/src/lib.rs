//! ax MCP server — thin translator between MCP tool invocations and
//! the ax daemon's Unix-socket envelope protocol. Mirrors the Go
//! `internal/mcpserver` package. The first slice ports the daemon
//! client so tool handlers have a stable async surface to call;
//! individual tool registrations land in follow-up slices.

#![forbid(unsafe_code)]

mod daemon_client;
mod memory_scope;
mod server;
mod telemetry;

pub use daemon_client::{
    DaemonClient, DaemonClientBuilder, DaemonClientError, DEFAULT_REQUEST_TIMEOUT,
};
pub use memory_scope::find_effective_config;
pub use server::{run_stdio, Server};
pub use telemetry::{TelemetryEvent, TelemetrySink};
