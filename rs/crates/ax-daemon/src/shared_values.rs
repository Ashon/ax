//! In-memory shared-values store with JSON persistence. Mirrors the
//! `sharedValues` / `sharedPath` portion of
//! `internal/daemon/daemon.go`. Values are a flat `BTreeMap<String,
//! String>` — ordered, so persisted JSON key order matches Go's
//! (encoding/json sorts map keys alphabetically).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::atomicfile::{write_file_atomic, AtomicFileError};

pub(crate) const DEFAULT_FILE: &str = "shared_values.json";

#[derive(Debug, thiserror::Error)]
pub(crate) enum SharedError {
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
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("persist: {0}")]
    Persist(#[from] AtomicFileError),
}

/// Thread-safe key/value store with optional on-disk JSON.
#[derive(Debug)]
pub struct SharedValues {
    path: Option<PathBuf>,
    inner: RwLock<BTreeMap<String, String>>,
}

impl SharedValues {
    /// Create an in-memory-only store; nothing persists.
    #[must_use]
    pub fn in_memory() -> Arc<Self> {
        Arc::new(Self {
            path: None,
            inner: RwLock::new(BTreeMap::new()),
        })
    }

    /// Load the JSON file at `path` (if present) and return a store
    /// that writes back to the same path on every `set`. Missing /
    /// empty file → empty store.
    pub(crate) fn load(path: PathBuf) -> Result<Arc<Self>, SharedError> {
        let initial =
            match std::fs::read(&path) {
                Ok(bytes) if bytes.is_empty() => BTreeMap::new(),
                Ok(bytes) => serde_json::from_slice::<BTreeMap<String, String>>(&bytes).map_err(
                    |source| SharedError::Decode {
                        path: path.clone(),
                        source,
                    },
                )?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(source) => return Err(SharedError::Read { path, source }),
            };
        Ok(Arc::new(Self {
            path: Some(path),
            inner: RwLock::new(initial),
        }))
    }

    pub(crate) fn set(&self, key: &str, value: &str) -> Result<(), SharedError> {
        let snapshot = {
            let mut map = self.inner.write().expect("shared store poisoned");
            map.insert(key.to_owned(), value.to_owned());
            map.clone()
        };
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec(&snapshot)?;
            write_file_atomic(path, &bytes)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<String> {
        self.inner
            .read()
            .expect("shared store poisoned")
            .get(key)
            .cloned()
    }

    #[must_use]
    pub fn list(&self) -> BTreeMap<String, String> {
        self.inner.read().expect("shared store poisoned").clone()
    }
}

impl Default for SharedValues {
    fn default() -> Self {
        Self {
            path: None,
            inner: RwLock::new(BTreeMap::new()),
        }
    }
}

/// Build the default path under a daemon state dir.
#[must_use]
pub(crate) fn default_path(state_dir: &Path) -> PathBuf {
    state_dir.join(DEFAULT_FILE)
}
