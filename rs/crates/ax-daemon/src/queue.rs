//! In-memory FIFO message queue per workspace. MVP slice: no on-disk
//! persistence and no deduplication heuristics; just enough semantics to
//! support `send_message` → (push if connected) → `read_messages` drain
//! behaviour the Go queue exposes at `internal/daemon/msgqueue.go`.
//!
//! Later slices will layer on:
//!   - Persistent JSON log recovery (`msgqueue.go::Load`).
//!   - Filtering by `TaskID` for claim/release semantics.
//!   - Fresh-start suppression (`freshTaskDeliveryHeld`).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use uuid::Uuid;

use ax_proto::types::Message;

use crate::task_helpers::extract_task_id;

#[derive(Debug, Default)]
pub struct MessageQueue {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    by_workspace: BTreeMap<String, VecDeque<Message>>,
}

impl MessageQueue {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Append `msg` to `msg.to`'s inbox. Stamps a fresh id + timestamp
    /// if they were left blank, matching Go's enqueue path, and returns
    /// the finalised message so callers can forward the same shape in
    /// push envelopes.
    pub fn enqueue(&self, mut msg: Message) -> Message {
        if msg.id.is_empty() {
            msg.id = format!("msg-{}", Uuid::new_v4());
        }
        if msg.created_at.timestamp() == 0 {
            msg.created_at = Utc::now();
        }
        let to = msg.to.clone();
        let mut inner = self.inner.lock().expect("queue poisoned");
        inner
            .by_workspace
            .entry(to)
            .or_default()
            .push_back(msg.clone());
        msg
    }

    /// Pop up to `limit` messages destined for `workspace`, optionally
    /// filtered by sender. `limit <= 0` is treated as the Go default of
    /// 10 at the handler layer; this method takes an already-clamped
    /// count.
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
        out
    }

    /// Pending count for `workspace`. Used by handlers that need to
    /// know whether a wake should remain scheduled after a drain.
    #[must_use]
    pub fn pending_count(&self, workspace: &str) -> usize {
        let inner = self.inner.lock().expect("queue poisoned");
        inner.by_workspace.get(workspace).map_or(0, VecDeque::len)
    }

    /// Drop every pending message linked to `task_id` for `workspace`.
    /// Mirrors Go's `MessageQueue.RemoveTaskMessages`; returns the
    /// number of dropped messages so callers can log the cleanup.
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
        before - queue.len()
    }

    /// Return true when `workspace` has at least one pending message
    /// tagged with `task_id`.
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
}

fn message_task_id(msg: &Message) -> String {
    let trimmed = msg.task_id.trim();
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }
    extract_task_id(&msg.content)
}
