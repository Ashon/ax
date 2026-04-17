//! Per-workspace FIFO message queue with optional on-disk JSON
//! snapshot. Mirrors `internal/daemon/msgqueue.go`:
//!
//!   - In-memory `BTreeMap<workspace, VecDeque<Message>>` for
//!     `send_message` → (push if connected) → `read_messages` drain.
//!   - Default cap of 1000 pending messages per workspace; oldest
//!     entries are dropped when the cap is exceeded.
//!   - Optional persistence via `<state>/queue.json`. Writes are
//!     batched by the background flusher spawned from
//!     [`MessageQueue::spawn_flusher`] (100ms interval, matching Go's
//!     `defaultQueueFlushInterval`), so a perpetually-dirty queue
//!     does not issue an fs write on every enqueue.
//!   - `remove_task_messages` / `has_task_message` for task-tagged
//!     cleanup during cancel/remove/intervene.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use ax_proto::types::Message;

use crate::atomicfile::write_file_atomic;
use crate::task_helpers::extract_task_id;

pub const DEFAULT_MAX_QUEUE_PER_WORKSPACE: usize = 1000;
pub(crate) const DEFAULT_QUEUE_FLUSH_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const QUEUE_FILE: &str = "queue.json";

#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("read {path:?}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decode {path:?}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("encode queue: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("persist queue: {0}")]
    Persist(String),
}

#[derive(Debug)]
pub struct MessageQueue {
    file_path: Option<PathBuf>,
    max_size: Mutex<usize>,
    inner: Mutex<Inner>,
    persist_lock: Mutex<()>,
}

#[derive(Debug, Default)]
struct Inner {
    by_workspace: BTreeMap<String, VecDeque<Message>>,
    dirty: bool,
}

impl MessageQueue {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            file_path: None,
            max_size: Mutex::new(DEFAULT_MAX_QUEUE_PER_WORKSPACE),
            inner: Mutex::new(Inner::default()),
            persist_lock: Mutex::new(()),
        })
    }

    /// Build a queue that persists dirty snapshots to
    /// `<state_dir>/queue.json`. Loads an existing snapshot if
    /// present; missing / empty file is treated as an empty queue.
    pub fn load(state_dir: &Path) -> Result<Arc<Self>, QueueError> {
        let path = state_dir.join(QUEUE_FILE);
        let by_workspace = match std::fs::read(&path) {
            Ok(bytes) if bytes.is_empty() => BTreeMap::new(),
            Ok(bytes) => {
                let map: BTreeMap<String, VecDeque<Message>> = serde_json::from_slice(&bytes)
                    .map_err(|source| QueueError::Decode {
                        path: path.clone(),
                        source,
                    })?;
                map
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(source) => return Err(QueueError::Read { path, source }),
        };
        Ok(Arc::new(Self {
            file_path: Some(path),
            max_size: Mutex::new(DEFAULT_MAX_QUEUE_PER_WORKSPACE),
            inner: Mutex::new(Inner {
                by_workspace,
                dirty: false,
            }),
            persist_lock: Mutex::new(()),
        }))
    }

    /// Override the per-workspace cap. Values ≤ 0 disable the cap —
    /// primarily used by tests that want to assert queue-cap drops in
    /// isolation.
    pub fn set_max_size(&self, n: usize) {
        *self.max_size.lock().expect("queue max_size poisoned") = n;
    }

    /// Append `msg` to `msg.to`'s inbox. Stamps a fresh id +
    /// timestamp if they were left blank, matching Go's enqueue path,
    /// and returns the finalised message so callers can forward the
    /// same shape in push envelopes. Dirty bit is toggled so the
    /// next flush picks up the change.
    pub fn enqueue(&self, mut msg: Message) -> Message {
        if msg.id.is_empty() {
            msg.id = format!("msg-{}", Uuid::new_v4());
        }
        if msg.created_at.timestamp() == 0 {
            msg.created_at = Utc::now();
        }
        let to = msg.to.clone();
        let max = *self.max_size.lock().expect("queue max_size poisoned");
        let mut inner = self.inner.lock().expect("queue poisoned");
        let queue = inner.by_workspace.entry(to.clone()).or_default();
        queue.push_back(msg.clone());
        if max > 0 && queue.len() > max {
            let drop_count = queue.len() - max;
            for _ in 0..drop_count {
                queue.pop_front();
            }
            tracing::warn!(
                workspace = %to,
                dropped = drop_count,
                "queue cap exceeded, dropping oldest messages"
            );
        }
        inner.dirty = true;
        msg
    }

    /// Pop up to `limit` messages destined for `workspace`,
    /// optionally filtered by sender. `limit <= 0` is treated as the
    /// Go default of 10 at the handler layer; this method takes an
    /// already-clamped count.
    pub fn dequeue(&self, workspace: &str, limit: usize, from: Option<&str>) -> Vec<Message> {
        let mut inner = self.inner.lock().expect("queue poisoned");
        let Some(queue) = inner.by_workspace.get_mut(workspace) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(limit.min(queue.len()));
        let mut kept = VecDeque::with_capacity(queue.len());
        while let Some(msg) = queue.pop_front() {
            if out.len() >= limit {
                kept.push_back(msg);
                continue;
            }
            let keep = match from {
                Some(sender) => msg.from != sender,
                None => false,
            };
            if keep {
                kept.push_back(msg);
            } else {
                out.push(msg);
            }
        }
        kept.extend(queue.drain(..));
        *queue = kept;
        if !out.is_empty() {
            inner.dirty = true;
        }
        out
    }

    #[must_use]
    pub fn pending_count(&self, workspace: &str) -> usize {
        let inner = self.inner.lock().expect("queue poisoned");
        inner.by_workspace.get(workspace).map_or(0, VecDeque::len)
    }

    pub fn remove_task_messages(&self, workspace: &str, task_id: &str) -> usize {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return 0;
        }
        let mut inner = self.inner.lock().expect("queue poisoned");
        let Some(queue) = inner.by_workspace.get_mut(workspace) else {
            return 0;
        };
        let before = queue.len();
        queue.retain(|msg| message_task_id(msg) != task_id);
        let removed = before - queue.len();
        if removed > 0 {
            inner.dirty = true;
        }
        removed
    }

    #[must_use]
    pub fn has_task_message(&self, workspace: &str, task_id: &str) -> bool {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return false;
        }
        let inner = self.inner.lock().expect("queue poisoned");
        inner
            .by_workspace
            .get(workspace)
            .is_some_and(|queue| queue.iter().any(|msg| message_task_id(msg) == task_id))
    }

    /// Write the dirty snapshot to disk if the queue has a persistent
    /// path. Returns `Ok(())` when nothing needed to be written or
    /// when the write succeeded. On write failure the dirty flag is
    /// re-set so the next flush retries, matching Go.
    pub fn flush(&self) -> Result<(), QueueError> {
        let Some(path) = self.file_path.as_ref() else {
            return Ok(());
        };
        let _persist = self.persist_lock.lock().expect("queue persist poisoned");

        let snapshot: Option<BTreeMap<String, VecDeque<Message>>> = {
            let mut inner = self.inner.lock().expect("queue poisoned");
            if inner.dirty {
                inner.dirty = false;
                Some(clone_pending(&inner.by_workspace))
            } else {
                None
            }
        };
        let Some(snapshot) = snapshot else {
            return Ok(());
        };

        let bytes = serde_json::to_vec(&snapshot)?;
        if let Err(source) = write_file_atomic(path, &bytes) {
            // Re-arm the dirty flag so the next tick retries.
            self.inner.lock().expect("queue poisoned").dirty = true;
            return Err(QueueError::Persist(source.to_string()));
        }
        Ok(())
    }

    /// Spawn a background tokio task that calls [`Self::flush`]
    /// every `DEFAULT_QUEUE_FLUSH_INTERVAL` until the returned
    /// [`FlusherHandle`] is shut down. No-op for in-memory queues.
    pub fn spawn_flusher(self: &Arc<Self>) -> FlusherHandle {
        if self.file_path.is_none() {
            return FlusherHandle::disabled();
        }
        let queue = self.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_task = stop.clone();
        let join = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(DEFAULT_QUEUE_FLUSH_INTERVAL);
            // Skip the immediate first tick so we don't double-flush on startup.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if let Err(e) = queue.flush() {
                    tracing::warn!(error = %e, "queue flush failed");
                }
                if stop_task.load(Ordering::Relaxed) {
                    // Final flush to capture any mutations since the last tick.
                    if let Err(e) = queue.flush() {
                        tracing::warn!(error = %e, "queue final flush failed");
                    }
                    break;
                }
            }
        });
        FlusherHandle {
            stop,
            join: Some(join),
        }
    }
}

fn clone_pending(src: &BTreeMap<String, VecDeque<Message>>) -> BTreeMap<String, VecDeque<Message>> {
    let mut out = BTreeMap::new();
    for (workspace, queue) in src {
        out.insert(workspace.clone(), queue.clone());
    }
    out
}

fn message_task_id(msg: &Message) -> String {
    let trimmed = msg.task_id.trim();
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }
    extract_task_id(&msg.content)
}

/// Returned by [`MessageQueue::spawn_flusher`]. Drop or explicitly
/// `shutdown()` to stop the background flusher and trigger a final
/// flush.
#[must_use = "flusher runs until shutdown"]
pub struct FlusherHandle {
    stop: Arc<AtomicBool>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl FlusherHandle {
    fn disabled() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            join: None,
        }
    }

    pub async fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

impl Drop for FlusherHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}
