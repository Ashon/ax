//! In-memory workspace registry. Each registered connection owns a
//! bounded mpsc outbox that the server's per-connection writer task
//! drains; `Send` returns false when the entry is closed or the outbox
//! backpressure window has expired.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use ax_proto::types::{AgentStatus, AgentStatusMetrics, WorkspaceInfo};
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
        let previous_info = inner.entries.get(workspace).map(|e| e.info.clone());
        let status_text = previous_info
            .as_ref()
            .map(|info| info.status_text.clone())
            .unwrap_or_default();
        let status_metrics = previous_info.and_then(|info| info.status_metrics);
        let info = WorkspaceInfo {
            name: workspace.to_owned(),
            dir: dir.to_owned(),
            description: description.to_owned(),
            status: AgentStatus::Online,
            status_text,
            status_metrics,
            git_status: None,
            connected_at: Some(now),
            last_activity_at: Some(now),
            active_task_count: 0,
            current_task_id: None,
            connection_generation: id,
            idle_timeout_seconds: i64::try_from(idle_timeout.as_secs()).unwrap_or(i64::MAX),
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
                info.connection_generation = e.id;
                info.idle_timeout_seconds =
                    i64::try_from(e.idle_timeout.as_secs()).unwrap_or(i64::MAX);
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

    /// Store a structured status metric snapshot for a registered
    /// workspace. The daemon-owned tmux-visible status string is derived
    /// from the same structured snapshot and exposed through
    /// `WorkspaceInfo::status_text`.
    pub fn update_status_metrics_at(
        &self,
        workspace: &str,
        mut metrics: AgentStatusMetrics,
        now: DateTime<Utc>,
    ) -> Option<AgentStatusMetrics> {
        let mut inner = self.inner.lock().expect("registry poisoned");
        let entry = inner.entries.get_mut(workspace)?;
        metrics.workspace = workspace.to_owned();
        if metrics.agent.is_empty() {
            metrics.agent = workspace.to_owned();
        }
        metrics.updated_at = Some(now);
        metrics.status_title = metrics.formatted_status_title();
        entry.info.status_text.clone_from(&metrics.status_title);
        entry.info.status_metrics = Some(metrics.clone());
        entry.last_active_at = now;
        Some(metrics)
    }

    #[must_use]
    pub fn get_status_metrics(&self, workspace: &str) -> Option<(AgentStatusMetrics, bool)> {
        let inner = self.inner.lock().expect("registry poisoned");
        let entry = inner.entries.get(workspace)?;
        Some(status_metrics_for_entry(entry))
    }

    #[must_use]
    pub fn list_status_metrics(&self) -> Vec<AgentStatusMetrics> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .entries
            .values()
            .map(|entry| status_metrics_for_entry(entry).0)
            .collect()
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

fn status_metrics_for_entry(entry: &Entry) -> (AgentStatusMetrics, bool) {
    match entry.info.status_metrics.clone() {
        Some(mut metrics) => {
            metrics.workspace = entry.info.name.clone();
            metrics.status_title = metrics.formatted_status_title();
            (metrics, true)
        }
        None => {
            let mut metrics = AgentStatusMetrics::unknown_for_workspace(&entry.info.name);
            metrics.last_activity_at = Some(entry.last_active_at);
            metrics.status_title = metrics.formatted_status_title();
            (metrics, false)
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

    #[test]
    fn status_metrics_update_sets_structured_snapshot_and_title() {
        let registry = Registry::new();
        let _ = registry.register("worker", "/tmp/worker", "", "/tmp/config.yaml");
        let marker = Utc
            .with_ymd_and_hms(2026, 4, 28, 7, 0, 0)
            .single()
            .expect("valid timestamp");

        let stored = registry
            .update_status_metrics_at(
                "worker",
                AgentStatusMetrics {
                    runtime_id: "codex".to_owned(),
                    context_tokens: Some(142_000),
                    context_window: Some(200_000),
                    work_state: ax_proto::types::AgentWorkState::Idle,
                    compact_eligible: Some(true),
                    ..AgentStatusMetrics::default()
                },
                marker,
            )
            .expect("stored metrics");

        assert_eq!(
            stored.status_title,
            "ax:worker ctx=142k/200k 71% idle compact=eligible"
        );
        let info = registry.list().pop().expect("registered workspace");
        assert_eq!(info.status_text, stored.status_title);
        assert_eq!(info.status_metrics, Some(stored));
        assert_eq!(info.last_activity_at, Some(marker));
    }

    #[test]
    fn status_metrics_query_degrades_to_unknown_snapshot() {
        let registry = Registry::new();
        let _ = registry.register("worker", "/tmp/worker", "", "/tmp/config.yaml");

        let (metrics, found) = registry
            .get_status_metrics("worker")
            .expect("registered workspace");

        assert!(!found);
        assert_eq!(metrics.workspace, "worker");
        assert_eq!(
            metrics.status_title,
            "ax:worker ctx=?/? ?% unknown compact=?"
        );
    }
}
