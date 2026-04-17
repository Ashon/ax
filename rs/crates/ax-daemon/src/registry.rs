//! In-memory workspace registry. Each registered connection owns a
//! bounded mpsc outbox that the server's per-connection writer task
//! drains; `Send` returns false when the entry is closed or the outbox
//! backpressure window has expired.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use ax_proto::types::{AgentStatus, WorkspaceInfo};
use ax_proto::Envelope;

/// Bounded outbox size; matches Go's `outboxCapacity = 256`.
pub(crate) const OUTBOX_CAPACITY: usize = 256;

/// Per-registration identity. The u64 is handed out by the registry so
/// callers can distinguish stale re-registrations from active ones.
pub(crate) type ConnectionId = u64;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: ConnectionId,
    pub info: WorkspaceInfo,
    pub config_path: String,
    pub outbox: mpsc::Sender<Envelope>,
}

impl Entry {
    /// Try to enqueue `env` for the per-connection writer task. Returns
    /// `false` when the outbox is full or closed.
    pub fn try_send(&self, env: Envelope) -> bool {
        self.outbox.try_send(env).is_ok()
    }
}

/// Outcome of `Registry::register` — the caller uses `previous` to close
/// any old connection that was evicted by the new registration.
pub struct RegisterOutcome {
    pub entry: Entry,
    pub receiver: mpsc::Receiver<Envelope>,
    pub previous: Option<Entry>,
}

#[derive(Debug, Default)]
pub struct Registry {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    next_id: u64,
    entries: BTreeMap<String, Entry>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Insert a fresh entry for `workspace`, evicting any prior entry
    /// and handing its outbox back to the caller so the old writer can
    /// be drained / closed.
    pub fn register(
        &self,
        workspace: &str,
        dir: &str,
        description: &str,
        config_path: &str,
    ) -> RegisterOutcome {
        let (tx, rx) = mpsc::channel(OUTBOX_CAPACITY);
        let mut inner = self.inner.lock().expect("registry poisoned");
        inner.next_id += 1;
        let id = inner.next_id;
        let status_text = inner
            .entries
            .get(workspace)
            .map(|e| e.info.status_text.clone())
            .unwrap_or_default();
        let info = WorkspaceInfo {
            name: workspace.to_owned(),
            dir: dir.to_owned(),
            description: description.to_owned(),
            status: AgentStatus::Online,
            status_text,
            connected_at: Some(Utc::now()),
        };
        let entry = Entry {
            id,
            info,
            config_path: config_path.to_owned(),
            outbox: tx,
        };
        let previous = inner.entries.insert(workspace.to_owned(), entry.clone());
        RegisterOutcome {
            entry,
            receiver: rx,
            previous,
        }
    }

    /// Remove `workspace` only if the currently-registered entry has
    /// `id`. Returns true on success so the caller knows whether to run
    /// disconnect-time cleanup.
    pub fn unregister_if(&self, workspace: &str, id: ConnectionId) -> bool {
        let mut inner = self.inner.lock().expect("registry poisoned");
        match inner.entries.get(workspace) {
            Some(entry) if entry.id == id => {
                inner.entries.remove(workspace);
                true
            }
            _ => false,
        }
    }

    /// Force-remove the entry regardless of connection id. Used when a
    /// client sends an explicit unregister envelope.
    pub fn unregister(&self, workspace: &str) {
        let mut inner = self.inner.lock().expect("registry poisoned");
        inner.entries.remove(workspace);
    }

    #[must_use]
    pub fn get(&self, workspace: &str) -> Option<Entry> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .entries
            .get(workspace)
            .cloned()
    }

    /// Snapshot of all currently-registered workspaces. Ordered by name
    /// (`BTreeMap`) which also matches Go's JSON output.
    #[must_use]
    pub fn list(&self) -> Vec<WorkspaceInfo> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .entries
            .values()
            .map(|e| e.info.clone())
            .collect()
    }

    /// Update the free-form status text for `workspace`. Returns false
    /// if the workspace isn't registered.
    pub fn set_status_text(&self, workspace: &str, text: &str) -> bool {
        let mut inner = self.inner.lock().expect("registry poisoned");
        match inner.entries.get_mut(workspace) {
            Some(entry) => {
                text.clone_into(&mut entry.info.status_text);
                true
            }
            None => false,
        }
    }

    /// Stamp `connected_at` forward to `now`. Match Go's `Touch` which
    /// bumps the last-active timestamp on outbound traffic; we fold it
    /// into `connected_at` for now since we don't store a separate
    /// `last_active_at` field yet.
    pub fn touch(&self, workspace: &str, now: DateTime<Utc>) {
        let mut inner = self.inner.lock().expect("registry poisoned");
        if let Some(entry) = inner.entries.get_mut(workspace) {
            entry.info.connected_at = Some(now);
        }
    }
}
