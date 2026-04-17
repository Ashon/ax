//! Async Unix-socket daemon that routes envelopes between workspace
//! agents. MVP slice: `register` / `unregister` / `send_message` /
//! `broadcast` / `read_messages` / `list_workspaces` / `set_status`. No
//! persistence, task store, memory, wake scheduler, or session manager
//! yet — those land in later slices as we work through the Go
//! `internal/daemon` surface.

#![forbid(unsafe_code)]

mod atomicfile;
mod handlers;
mod queue;
mod registry;
mod server;
mod shared_values;
mod socket_path;
mod usage_trends;

pub use queue::MessageQueue;
pub use registry::Registry;
pub use server::{Daemon, DaemonError, DaemonHandle};
pub use socket_path::{expand_socket_path, DEFAULT_SOCKET_PATH};
