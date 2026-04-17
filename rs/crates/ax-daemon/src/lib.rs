//! Async Unix-socket daemon that routes envelopes between workspace
//! agents. MVP slice: `register` / `unregister` / `send_message` /
//! `broadcast` / `read_messages` / `list_workspaces` / `set_status`. No
//! persistence, task store, memory, wake scheduler, or session manager
//! yet — those land in later slices as we work through the Go
//! `internal/daemon` surface.

#![forbid(unsafe_code)]

mod atomicfile;
mod handlers;
mod history;
mod memory;
mod queue;
mod registry;
mod server;
mod shared_values;
mod socket_path;
mod task_helpers;
mod task_store;
mod team_reconfigure;
mod team_state_store;
mod usage_trends;

pub use history::{History, HistoryEntry, HistoryError, DEFAULT_HISTORY_MAX_SIZE};
pub use queue::{FlusherHandle, MessageQueue, QueueError, DEFAULT_MAX_QUEUE_PER_WORKSPACE};
pub use registry::Registry;
pub use server::{Daemon, DaemonError, DaemonHandle};
pub use socket_path::{expand_socket_path, DEFAULT_SOCKET_PATH};
pub use task_store::{CreateTaskInput, TaskStore, TaskStoreError};
pub use team_reconfigure::{TeamController, TeamError};
pub use team_state_store::{TeamStateError, TeamStateStore};
