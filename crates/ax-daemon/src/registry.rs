//! In-memory workspace registry. Each registered connection owns a
//! bounded mpsc outbox that the server's per-connection writer task
//! drains; `Send` returns false when the entry is closed or the outbox
//! backpressure window has expired.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use ax_proto::types::{AgentStatus, WorkspaceInfo};
use ax_proto::Envelope;

/// Bounded outbox size.
pub(crate) const OUTBOX_CAPACITY: usize = 256;

/// Per-registration identity. The u64 is handed out by the registry so
/// callers can distinguish stale re-registrations from active ones.
pub(crate) type ConnectionId = u64;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: ConnectionId,
    pub info: WorkspaceInfo,
    pub config_path: String,
    pub idle_timeout: Duration,
    pub last_active_at: DateTime<Utc>,
    pub outbox: mpsc::Sender<Envelope>,
}

impl Entry {
    /// Try to enqueue `env` for the per-connection writer task. Returns
    /// `false` when the outbox is full or closed.
    pub fn try_send(&self, env: Envelope) -> bool {
        self.outbox.try_send(env).is_ok()
    }
}

/// Immutable view used by the idle-sleep guard — includes the knobs
/// `should_sleep` reads without leaking the mpsc outbox.
#[derive(Debug, Clone)]
pub struct RegisteredWorkspace {
    pub info: WorkspaceInfo,
    pub config_path: String,
    pub idle_timeout: Duration,
    pub last_active_at: DateTime<Utc>,
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
        self.register_with_idle(workspace, dir, description, config_path, Duration::ZERO)
    }

    /// Like [`Self::register`] but also records the idle timeout
    /// reported in the `RegisterPayload`. The idle-sleep guard uses
    /// this to decide whether a workspace is eligible for the
    /// `stop_idle` lifecycle when no pending work remains.
    pub fn register_with_idle(
        &self,
        workspace: &str,
        dir: &str,
        description: &str,
        config_path: &str,
        idle_timeout: Duration,
    ) -> RegisterOutcome {
        let (tx, rx) = mpsc::channel(OUTBOX_CAPACITY);
        let now = Utc::now();
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
            git_status: None,
            connected_at: Some(now),
            last_activity_at: Some(now),
            active_task_count: 0,
            current_task_id: None,
        };
        let entry = Entry {
            id,
            info,
            config_path: config_path.to_owned(),
            idle_timeout,
            last_active_at: now,
            outbox: tx,
        };
        let previous = inner.entries.insert(workspace.to_owned(), entry.clone());
        RegisterOutcome {
            entry,
            receiver: rx,
            previous,
        }
    }

    /// Snapshot of all currently-registered workspaces including
    /// idle metadata. Used by the idle-sleep loop; `list` stays as
    /// a thinner projection for `list_workspaces` responses.
    #[must_use]
    pub fn snapshot(&self) -> Vec<RegisteredWorkspace> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .entries
            .values()
            .map(|e| RegisteredWorkspace {
                info: e.info.clone(),
                config_path: e.config_path.clone(),
                idle_timeout: e.idle_timeout,
                last_active_at: e.last_active_at,
            })
            .collect()
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
    /// (`BTreeMap`) so JSON rendering is stable. Splices the registry's
    /// authoritative `last_active_at` watermark into each snapshot so
    /// consumers see one source of truth for liveness timestamps.
    #[must_use]
    pub fn list(&self) -> Vec<WorkspaceInfo> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .entries
            .values()
            .map(|e| {
                let mut info = e.info.clone();
                info.last_activity_at = Some(e.last_active_at);
                info
            })
            .collect()
    }

    /// Update the free-form status text for `workspace` and refresh
    /// its activity watermark. A status heartbeat is a daemon-visible
    /// sign of recent agent activity, so idle sleep must not race it.
    pub fn set_status_text(&self, workspace: &str, text: &str) -> bool {
        self.set_status_text_at(workspace, text, Utc::now())
    }

    /// Test hook / deterministic variant of [`Self::set_status_text`].
    pub fn set_status_text_at(&self, workspace: &str, text: &str, now: DateTime<Utc>) -> bool {
        let mut inner = self.inner.lock().expect("registry poisoned");
        match inner.entries.get_mut(workspace) {
            Some(entry) => {
                text.clone_into(&mut entry.info.status_text);
                entry.last_active_at = now;
                true
            }
            None => false,
        }
    }

    /// Bump the last-active watermark on outbound traffic. Mirrors
    /// `Touch`; `connected_at` stays pinned to the initial
    /// registration time so clients can tell "since when has this
    /// workspace been online" apart from "most recent activity".
    pub fn touch(&self, workspace: &str, now: DateTime<Utc>) {
        let mut inner = self.inner.lock().expect("registry poisoned");
        if let Some(entry) = inner.entries.get_mut(workspace) {
            entry.last_active_at = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn status_text_update_refreshes_activity_watermark() {
        let registry = Registry::new();
        let _ = registry.register_with_idle(
            "worker",
            "/tmp/worker",
            "",
            "/tmp/config.yaml",
            Duration::from_secs(900),
        );
        let marker = Utc
            .with_ymd_and_hms(2026, 4, 21, 5, 0, 0)
            .single()
            .expect("valid timestamp");

        assert!(registry.set_status_text_at("worker", "busy", marker));
        let snapshot = registry.snapshot();

        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].info.status_text, "busy");
        assert_eq!(snapshot[0].last_active_at, marker);
    }
}
