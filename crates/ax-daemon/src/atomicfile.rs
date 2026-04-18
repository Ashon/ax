//! Atomic write for persisted JSON state. Writes to a sibling
//! `.<name>.tmp-<rand>` file under the same directory, fsyncs, then
//! renames in place so readers (or a crashed daemon being restarted)
//! never observe a half-written file. Mirrors
//! `internal/daemon/atomicfile.go`.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AtomicFileError {
    #[error("empty path")]
    EmptyPath,
    #[error("ensure dir {path:?}: {source}")]
    EnsureDir {
        path: std::path::PathBuf,
        source: io::Error,
    },
    #[error("create temp file under {path:?}: {source}")]
    CreateTemp {
        path: std::path::PathBuf,
        source: io::Error,
    },
    #[error("write temp file: {0}")]
    Write(io::Error),
    #[error("sync temp file: {0}")]
    Sync(io::Error),
    #[error("rename temp file into {path:?}: {source}")]
    Rename {
        path: std::path::PathBuf,
        source: io::Error,
    },
}

/// Write `data` to `path` atomically. `dir` ancestors are created when
/// they don't exist. The temp file is removed on any failure path.
pub(crate) fn write_file_atomic(path: &Path, data: &[u8]) -> Result<(), AtomicFileError> {
    if path.as_os_str().is_empty() {
        return Err(AtomicFileError::EmptyPath);
    }
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf);
    fs::create_dir_all(&dir).map_err(|source| AtomicFileError::EnsureDir {
        path: dir.clone(),
        source,
    })?;

    let base = path
        .file_name()
        .and_then(|os| os.to_str())
        .unwrap_or("file");
    let suffix: u64 = {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
    };
    let tmp_path = dir.join(format!(".{base}.tmp-{suffix:x}"));

    // Helper to remove the temp file on every error branch.
    let cleanup = |tmp: &Path| {
        let _ = fs::remove_file(tmp);
    };

    let mut tmp = File::create(&tmp_path).map_err(|source| AtomicFileError::CreateTemp {
        path: dir.clone(),
        source,
    })?;
    if let Err(e) = tmp.write_all(data) {
        drop(tmp);
        cleanup(&tmp_path);
        return Err(AtomicFileError::Write(e));
    }
    if let Err(e) = tmp.sync_all() {
        drop(tmp);
        cleanup(&tmp_path);
        return Err(AtomicFileError::Sync(e));
    }
    drop(tmp);

    fs::rename(&tmp_path, path).map_err(|source| {
        cleanup(&tmp_path);
        AtomicFileError::Rename {
            path: path.to_owned(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_full_file_and_replaces_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("state.json");
        write_file_atomic(&path, b"v1").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"v1");
        write_file_atomic(&path, b"v2").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"v2");
    }

    #[test]
    fn empty_path_is_rejected() {
        let err = write_file_atomic(Path::new(""), b"x").unwrap_err();
        assert!(matches!(err, AtomicFileError::EmptyPath));
    }
}
