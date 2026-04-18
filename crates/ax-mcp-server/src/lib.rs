//! ax MCP server — thin translator between MCP tool invocations and
//! the ax daemon's Unix-socket envelope protocol.

#![forbid(unsafe_code)]

mod daemon_client;
mod memory_scope;
mod planner;
mod server;
mod telemetry;

pub use daemon_client::{
    DaemonClient, DaemonClientBuilder, DaemonClientError, DEFAULT_REQUEST_TIMEOUT,
};
pub use memory_scope::find_effective_config;
pub use planner::{
    plan_initial_team, plan_team_reconfigure, DirSummary, InitialTeamPlan, ReconfigureTeamPlan,
    WorkspaceSummary,
};
pub use server::{run_stdio, Server};
pub use telemetry::{TelemetryEvent, TelemetrySink};
