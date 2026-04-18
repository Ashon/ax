//! Async Unix-socket daemon that routes envelopes between workspace
//! agents. Covers registry, message queue + history, durable memory,
//! task store, wake scheduler, session manager, and team-reconfigure
//! controller.

#![forbid(unsafe_code)]

mod atomicfile;
mod daemonutil;
mod handlers;
mod history;
mod memory;
mod queue;
mod registry;
mod server;
mod session_manager;
mod shared_values;
mod socket_path;
mod task_helpers;
mod task_store;
mod team_reconfigure;
mod team_state_store;
mod usage_trends;
mod wake_scheduler;

pub use daemonutil::wake_prompt;
pub use history::{History, HistoryEntry, HistoryError, DEFAULT_HISTORY_MAX_SIZE};
pub use queue::{FlusherHandle, MessageQueue, QueueError, DEFAULT_MAX_QUEUE_PER_WORKSPACE};
pub use registry::Registry;
pub use server::{Daemon, DaemonError, DaemonHandle};
pub use session_manager::{SessionManager, SessionManagerError};
pub use socket_path::{expand_socket_path, DEFAULT_SOCKET_PATH};
pub use task_store::{CreateTaskInput, TaskStore, TaskStoreError};
pub use team_reconfigure::{TeamController, TeamError};
pub use team_state_store::{TeamStateError, TeamStateStore};
pub use wake_scheduler::{
    wake_backoff, RealWakeBackend, WakeBackend, WakeLoopHandle, WakeScheduler, WakeState,
    WAKE_CHECK_INTERVAL, WAKE_MAX_ATTEMPTS,
};
