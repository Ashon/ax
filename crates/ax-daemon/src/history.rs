//! Append-only message history with a JSONL backing file. Each call
//! to [`History::append`] writes one line to `message_history.jsonl`
//! in the daemon state dir, keeping the last `max_size` entries in
//! memory for `Recent` / `RecentMatching` queries (powering
//! `list_workspaces` status snippets and task-observability checks).

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use ax_proto::types::Message;

pub(crate) const HISTORY_FILE: &str = "message_history.jsonl";

/// Default ring-buffer capacity used by production daemon wiring.
pub const DEFAULT_HISTORY_MAX_SIZE: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
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
    #[error("append {path:?}: {source}")]
    Append {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("encode history entry: {0}")]
    Encode(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    #[serde(rename = "ts")]
    pub timestamp: DateTime<Utc>,
    pub from: String,
    pub to: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
}

#[derive(Debug)]
pub struct History {
    file_path: Option<PathBuf>,
    max_size: usize,
    inner: Mutex<VecDeque<HistoryEntry>>,
}

impl History {
    #[must_use]
    pub fn in_memory(max_size: usize) -> Arc<Self> {
        Arc::new(Self {
            file_path: None,
            max_size: max_size.max(1),
            inner: Mutex::new(VecDeque::new()),
        })
    }

    /// Open a persistent history under `state_dir`. Missing file is
    /// treated as an empty buffer. On-disk lines beyond `max_size` are
    /// discarded from the in-memory tail on load.
    pub fn load(state_dir: &Path, max_size: usize) -> Result<Arc<Self>, HistoryError> {
        let path = state_dir.join(HISTORY_FILE);
        let max_size = max_size.max(1);
        let mut buf: VecDeque<HistoryEntry> = VecDeque::new();
        match std::fs::read(&path) {
            Ok(bytes) if bytes.is_empty() => {}
            Ok(bytes) => {
                for line in bytes.split(|b| *b == b'\n') {
                    if line.is_empty() {
                        continue;
                    }
                    let entry: HistoryEntry =
                        serde_json::from_slice(line).map_err(|source| HistoryError::Decode {
                            path: path.clone(),
                            source,
                        })?;
                    if buf.len() >= max_size {
                        buf.pop_front();
                    }
                    buf.push_back(entry);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(HistoryError::Read { path, source }),
        }
        Ok(Arc::new(Self {
            file_path: Some(path),
            max_size,
            inner: Mutex::new(buf),
        }))
    }

    /// Append a message to the ring buffer and to the on-disk JSONL
    /// log. Persistence failures are best-effort — they're logged and
    /// swallowed so one bad fs state can't stall the hot path.
    pub fn append_message(&self, msg: &Message) {
        self.append(&HistoryEntry {
            timestamp: Utc::now(),
            from: msg.from.clone(),
            to: msg.to.clone(),
            content: msg.content.clone(),
            task_id: msg.task_id.clone(),
        });
    }

    pub fn append(&self, entry: &HistoryEntry) {
        let mut inner = self.inner.lock().expect("history poisoned");
        if inner.len() >= self.max_size {
            inner.pop_front();
        }
        inner.push_back(entry.clone());
        drop(inner);
        if let Some(path) = &self.file_path {
            if let Err(e) = append_entry_to_file(path, entry) {
                tracing::warn!(error = %e, "append history entry");
            }
        }
    }

    #[must_use]
    pub fn recent(&self, n: usize) -> Vec<HistoryEntry> {
        if n == 0 {
            return Vec::new();
        }
        let inner = self.inner.lock().expect("history poisoned");
        let start = inner.len().saturating_sub(n);
        inner.iter().skip(start).cloned().collect()
    }

    pub fn recent_matching<F>(&self, n: usize, mut matches: F) -> Vec<HistoryEntry>
    where
        F: FnMut(&HistoryEntry) -> bool,
    {
        if n == 0 {
            return Vec::new();
        }
        let inner = self.inner.lock().expect("history poisoned");
        let mut out: Vec<HistoryEntry> = Vec::with_capacity(n);
        for entry in inner.iter().rev() {
            if matches(entry) {
                out.push(entry.clone());
                if out.len() == n {
                    break;
                }
            }
        }
        out
    }
}

fn append_entry_to_file(path: &Path, entry: &HistoryEntry) -> Result<(), HistoryError> {
    let mut bytes = serde_json::to_vec(entry)?;
    bytes.push(b'\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| HistoryError::Append {
            path: path.to_path_buf(),
            source,
        })?;
    f.write_all(&bytes).map_err(|source| HistoryError::Append {
        path: path.to_path_buf(),
        source,
    })
}
