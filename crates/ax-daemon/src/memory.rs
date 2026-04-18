//! Durable-memory store backing the `remember_memory` and
//! `recall_memories` handlers. In-memory `BTreeMap<String, Memory>`
//! keyed by UUID, persisted as a sorted `Vec<Memory>` JSON array via
//! the shared `write_file_atomic` helper.
//!
//! Mirrors `internal/memory/store.go`. Behaviours pinned here:
//!
//!   - `Remember` atomically updates supersedes-target pointers and
//!     rolls them back on persist failure.
//!   - `List` filters by scope / kind / tag / superseded, sorts active
//!     entries first, then most-recently updated, and applies a limit.
//!   - Scope / kind / tag normalisation matches Go verbatim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use ax_proto::types::Memory;

use crate::atomicfile::write_file_atomic;

struct SupersedeSnapshot {
    superseded_at: Option<DateTime<Utc>>,
    superseded_by: String,
    updated_at: DateTime<Utc>,
}

pub(crate) const FILE_NAME: &str = "memories.json";
pub(crate) const DEFAULT_KIND: &str = "fact";
pub(crate) const DEFAULT_LIMIT: i64 = 10;

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub scopes: Vec<String>,
    pub kind: String,
    pub tags: Vec<String>,
    pub include_superseded: bool,
    pub limit: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("memory scope is required")]
    ScopeRequired,
    #[error("memory content is required")]
    ContentRequired,
    #[error("memory created_by is required")]
    CreatedByRequired,
    #[error("memory {0:?} not found")]
    NotFound(String),
    #[error("memory {0:?} is already superseded")]
    AlreadySuperseded(String),
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
    #[error("encode memory store: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("persist memory store: {0}")]
    Persist(String),
}

#[derive(Debug)]
pub struct Store {
    file_path: Option<PathBuf>,
    inner: Mutex<BTreeMap<String, Memory>>,
}

impl Store {
    #[must_use]
    pub fn in_memory() -> Arc<Self> {
        Arc::new(Self {
            file_path: None,
            inner: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn load(state_dir: &Path) -> Result<Arc<Self>, MemoryError> {
        let path = state_dir.join(FILE_NAME);
        let map = match std::fs::read(&path) {
            Ok(bytes) if bytes.is_empty() => BTreeMap::new(),
            Ok(bytes) => {
                let entries: Vec<Memory> =
                    serde_json::from_slice(&bytes).map_err(|source| MemoryError::Decode {
                        path: path.clone(),
                        source,
                    })?;
                entries.into_iter().map(|m| (m.id.clone(), m)).collect()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(source) => return Err(MemoryError::Read { path, source }),
        };
        Ok(Arc::new(Self {
            file_path: Some(path),
            inner: Mutex::new(map),
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn remember(
        &self,
        scope: &str,
        kind: &str,
        subject: &str,
        content: &str,
        tags: &[String],
        created_by: &str,
        supersedes: &[String],
    ) -> Result<Memory, MemoryError> {
        let scope = normalize_scope(scope);
        let kind = normalize_kind(kind);
        let subject = subject.trim();
        let content = content.trim();
        let created_by = created_by.trim();
        let tags = normalize_tags(tags);
        let supersedes = normalize_ids(supersedes);

        if scope.is_empty() {
            return Err(MemoryError::ScopeRequired);
        }
        if content.is_empty() {
            return Err(MemoryError::ContentRequired);
        }
        if created_by.is_empty() {
            return Err(MemoryError::CreatedByRequired);
        }

        let now = Utc::now();
        let entry = Memory {
            id: Uuid::new_v4().to_string(),
            scope,
            kind,
            subject: subject.to_owned(),
            content: content.to_owned(),
            tags,
            created_by: created_by.to_owned(),
            supersedes: supersedes.clone(),
            superseded_by: String::new(),
            superseded_at: None,
            created_at: now,
            updated_at: now,
        };

        let mut inner = self.inner.lock().expect("memory store poisoned");

        // Pre-validate supersedes targets, collect rollback snapshots.
        let mut snapshots: BTreeMap<String, SupersedeSnapshot> = BTreeMap::new();
        for id in &supersedes {
            let target = inner
                .get(id)
                .ok_or_else(|| MemoryError::NotFound(id.clone()))?;
            if target.superseded_at.is_some() {
                return Err(MemoryError::AlreadySuperseded(id.clone()));
            }
            snapshots.insert(
                id.clone(),
                SupersedeSnapshot {
                    superseded_at: target.superseded_at,
                    superseded_by: target.superseded_by.clone(),
                    updated_at: target.updated_at,
                },
            );
        }
        for id in &supersedes {
            if let Some(target) = inner.get_mut(id) {
                target.superseded_at = Some(now);
                target.superseded_by.clone_from(&entry.id);
                target.updated_at = now;
            }
        }
        inner.insert(entry.id.clone(), entry.clone());

        if let Err(e) = self.persist_locked(&inner) {
            // Roll back the supersede edits and the new insertion.
            inner.remove(&entry.id);
            for (id, snap) in snapshots {
                if let Some(target) = inner.get_mut(&id) {
                    target.superseded_at = snap.superseded_at;
                    target.superseded_by = snap.superseded_by;
                    target.updated_at = snap.updated_at;
                }
            }
            return Err(e);
        }
        Ok(entry)
    }

    pub fn list(&self, query: &Query) -> Vec<Memory> {
        let scopes = normalize_scopes(&query.scopes);
        let use_kind = !query.kind.trim().is_empty();
        let kind = normalize_kind(&query.kind);
        let tags = normalize_tags(&query.tags);
        let limit = if query.limit <= 0 {
            DEFAULT_LIMIT
        } else {
            query.limit
        };

        let inner = self.inner.lock().expect("memory store poisoned");
        let mut result: Vec<Memory> = inner
            .values()
            .filter(|entry| {
                if !query.include_superseded && entry.superseded_at.is_some() {
                    return false;
                }
                if !scopes.is_empty() && !scopes.contains(&entry.scope) {
                    return false;
                }
                if use_kind && entry.kind != kind {
                    return false;
                }
                if !tags.is_empty() && !has_any_tag(&entry.tags, &tags) {
                    return false;
                }
                true
            })
            .cloned()
            .collect();

        result.sort_by(|a, b| {
            let a_active = a.superseded_at.is_none();
            let b_active = b.superseded_at.is_none();
            if a_active != b_active {
                return if a_active {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        if limit > 0 && result.len() > limit as usize {
            result.truncate(limit as usize);
        }
        result
    }

    fn persist_locked(&self, map: &BTreeMap<String, Memory>) -> Result<(), MemoryError> {
        let Some(path) = &self.file_path else {
            return Ok(());
        };
        let mut entries: Vec<Memory> = map.values().cloned().collect();
        entries.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let bytes = serde_json::to_vec(&entries)?;
        write_file_atomic(path, &bytes).map_err(|e| MemoryError::Persist(e.to_string()))
    }
}

// ---------- normalisation helpers ----------

#[must_use]
pub(crate) fn normalize_kind(kind: &str) -> String {
    let trimmed = kind.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        DEFAULT_KIND.to_owned()
    } else {
        trimmed
    }
}

#[must_use]
pub(crate) fn normalize_scope(scope: &str) -> String {
    let trimmed = scope.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.eq_ignore_ascii_case("global") {
        return "global".to_owned();
    }
    for prefix in ["project:", "workspace:", "task:"] {
        if trimmed.to_ascii_lowercase().starts_with(prefix) {
            let value = trimmed[prefix.len()..].trim();
            if prefix == "project:" {
                let v = if value.is_empty() { "root" } else { value };
                return format!("project:{v}");
            }
            if value.is_empty() {
                return String::new();
            }
            return format!("{prefix}{value}");
        }
    }
    trimmed.to_owned()
}

fn normalize_scopes(scopes: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(scopes.len());
    for s in scopes {
        let normalised = normalize_scope(s);
        if !normalised.is_empty() && !out.contains(&normalised) {
            out.push(normalised);
        }
    }
    out.sort();
    out
}

fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(tags.len());
    for t in tags {
        let trimmed = t.trim().to_ascii_lowercase();
        if !trimmed.is_empty() && !out.contains(&trimmed) {
            out.push(trimmed);
        }
    }
    out.sort();
    out
}

fn normalize_ids(ids: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let trimmed = id.trim();
        if !trimmed.is_empty() && !out.iter().any(|s: &String| s == trimmed) {
            out.push(trimmed.to_owned());
        }
    }
    out.sort();
    out
}

fn has_any_tag(haystack: &[String], needles: &[String]) -> bool {
    haystack.iter().any(|h| needles.iter().any(|n| h == n))
}
