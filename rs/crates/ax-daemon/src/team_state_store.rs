//! Persistent team-reconfigure state. Mirrors
//! `internal/daemon/teamstate_store.go`: the file contains a JSON
//! array of `TeamReconfigureState` entries keyed by `team_id`, with
//! atomic replacement on every put.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ax_proto::types::TeamReconfigureState;

use crate::atomicfile::write_file_atomic;

pub(crate) const TEAM_STATE_FILE: &str = "team_states.json";

#[derive(Debug, thiserror::Error)]
pub enum TeamStateError {
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
    #[error("encode team state: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("persist team state: {0}")]
    Persist(String),
}

#[derive(Debug)]
pub struct TeamStateStore {
    file_path: Option<PathBuf>,
    inner: Mutex<BTreeMap<String, TeamReconfigureState>>,
}

impl TeamStateStore {
    #[must_use]
    pub fn in_memory() -> Arc<Self> {
        Arc::new(Self {
            file_path: None,
            inner: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn load(state_dir: &Path) -> Result<Arc<Self>, TeamStateError> {
        let path = state_dir.join(TEAM_STATE_FILE);
        let map = match std::fs::read(&path) {
            Ok(bytes) if bytes.is_empty() => BTreeMap::new(),
            Ok(bytes) => {
                let entries: Vec<TeamReconfigureState> =
                    serde_json::from_slice(&bytes).map_err(|source| TeamStateError::Decode {
                        path: path.clone(),
                        source,
                    })?;
                entries
                    .into_iter()
                    .map(|s| (s.team_id.clone(), s))
                    .collect()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(source) => return Err(TeamStateError::Read { path, source }),
        };
        Ok(Arc::new(Self {
            file_path: Some(path),
            inner: Mutex::new(map),
        }))
    }

    pub fn get(&self, team_id: &str) -> Option<TeamReconfigureState> {
        self.inner
            .lock()
            .expect("team state store poisoned")
            .get(team_id)
            .cloned()
    }

    pub fn put(&self, state: TeamReconfigureState) -> Result<(), TeamStateError> {
        let mut inner = self.inner.lock().expect("team state store poisoned");
        inner.insert(state.team_id.clone(), state);
        self.persist_locked(&inner)
    }

    fn persist_locked(
        &self,
        inner: &BTreeMap<String, TeamReconfigureState>,
    ) -> Result<(), TeamStateError> {
        let Some(path) = &self.file_path else {
            return Ok(());
        };
        let entries: Vec<TeamReconfigureState> = inner.values().cloned().collect();
        let bytes = serde_json::to_vec(&entries)?;
        write_file_atomic(path, &bytes).map_err(|e| TeamStateError::Persist(e.to_string()))
    }
}
