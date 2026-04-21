//! Workspace-root git status collection for `list_workspaces`.
//!
//! Status collection shells out to git and can touch the filesystem
//! heavily on large repositories, so callers should go through
//! [`GitStatusCache`] instead of collecting on every UI refresh.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ax_proto::types::WorkspaceGitStatus;

pub(crate) const GIT_STATUS_CACHE_TTL: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) struct GitStatusCache {
    inner: Mutex<BTreeMap<String, CachedGitStatus>>,
}

#[derive(Debug, Clone)]
struct CachedGitStatus {
    checked_at: Instant,
    status: WorkspaceGitStatus,
}

#[derive(Debug, Default)]
struct StatusCounts {
    modified: i64,
    added: i64,
    deleted: i64,
    untracked: i64,
}

#[derive(Debug, Default)]
struct DiffStat {
    files: BTreeSet<String>,
    insertions: i64,
    deletions: i64,
}

impl GitStatusCache {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn status_for(&self, dir: &str) -> WorkspaceGitStatus {
        let key = dir.to_owned();
        {
            let inner = self.inner.lock().expect("git status cache poisoned");
            if let Some(cached) = inner.get(&key) {
                if cached.checked_at.elapsed() <= GIT_STATUS_CACHE_TTL {
                    return cached.status.clone();
                }
            }
        }

        let status = collect_git_status(Path::new(dir));
        self.inner
            .lock()
            .expect("git status cache poisoned")
            .insert(
                key,
                CachedGitStatus {
                    checked_at: Instant::now(),
                    status: status.clone(),
                },
            );
        status
    }
}

pub(crate) fn collect_git_status(dir: &Path) -> WorkspaceGitStatus {
    let Some(dir_str) = dir.to_str() else {
        return unavailable("inaccessible", "workspace dir is not valid UTF-8");
    };
    if dir_str.trim().is_empty() {
        return unavailable("inaccessible", "workspace dir is empty");
    }

    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return unavailable("inaccessible", "workspace dir is not a directory"),
        Err(e) => {
            return unavailable(
                "inaccessible",
                format!("workspace dir is not accessible: {e}"),
            )
        }
    }

    match git_output(dir, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(output) if output.status.success() => {
            if String::from_utf8_lossy(&output.stdout).trim() != "true" {
                return unavailable("non_git", "not inside a git work tree");
            }
        }
        Ok(output) => {
            let message = command_message(&output);
            let state = if is_not_git_message(&message) {
                "non_git"
            } else {
                "error"
            };
            return unavailable(state, message);
        }
        Err(e) => return unavailable("error", format!("run git: {e}")),
    }

    let status_output = match git_output(
        dir,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    ) {
        Ok(output) if output.status.success() => output,
        Ok(output) => return unavailable("error", command_message(&output)),
        Err(e) => return unavailable("error", format!("run git status: {e}")),
    };

    let counts = parse_status_porcelain_z(&status_output.stdout);
    let dirty =
        counts.modified > 0 || counts.added > 0 || counts.deleted > 0 || counts.untracked > 0;

    let mut status = WorkspaceGitStatus {
        state: if dirty { "dirty" } else { "clean" }.to_owned(),
        modified: counts.modified,
        added: counts.added,
        deleted: counts.deleted,
        untracked: counts.untracked,
        files_changed: 0,
        insertions: 0,
        deletions: 0,
        message: String::new(),
    };

    if dirty {
        add_diffstat(dir, &mut status);
    }

    status
}

fn unavailable(state: &str, message: impl Into<String>) -> WorkspaceGitStatus {
    WorkspaceGitStatus {
        state: state.to_owned(),
        modified: 0,
        added: 0,
        deleted: 0,
        untracked: 0,
        files_changed: 0,
        insertions: 0,
        deletions: 0,
        message: message.into(),
    }
}

fn git_output(dir: &Path, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
}

fn command_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        stderr
    }
}

fn is_not_git_message(message: &str) -> bool {
    message.contains("not a git repository") || message.contains("not a git directory")
}

fn parse_status_porcelain_z(bytes: &[u8]) -> StatusCounts {
    let mut counts = StatusCounts::default();
    let mut idx = 0;
    while idx < bytes.len() {
        let start = idx;
        while idx < bytes.len() && bytes[idx] != 0 {
            idx += 1;
        }
        let entry = &bytes[start..idx];
        idx = idx.saturating_add(1);
        if entry.len() < 2 {
            continue;
        }

        let x = entry[0];
        let y = entry[1];
        if x == b'?' && y == b'?' {
            counts.untracked += 1;
            continue;
        }
        if x == b'!' && y == b'!' {
            continue;
        }

        if x == b'D' || y == b'D' {
            counts.deleted += 1;
        } else if x == b'A' || y == b'A' {
            counts.added += 1;
        } else {
            counts.modified += 1;
        }

        if x == b'R' || x == b'C' {
            while idx < bytes.len() && bytes[idx] != 0 {
                idx += 1;
            }
            idx = idx.saturating_add(1);
        }
    }
    counts
}

fn add_diffstat(dir: &Path, status: &mut WorkspaceGitStatus) {
    let mut diffstat = DiffStat::default();
    collect_diffstat(
        dir,
        &["diff", "--numstat", "--no-renames", "--"],
        &mut diffstat,
    );
    collect_diffstat(
        dir,
        &["diff", "--cached", "--numstat", "--no-renames", "--"],
        &mut diffstat,
    );
    status.files_changed = i64::try_from(diffstat.files.len()).unwrap_or(i64::MAX);
    status.insertions = diffstat.insertions;
    status.deletions = diffstat.deletions;
}

fn collect_diffstat(dir: &Path, args: &[&str], diffstat: &mut DiffStat) {
    let Ok(output) = git_output(dir, args) else {
        return;
    };
    if !output.status.success() {
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(added) = parts.next() else {
            continue;
        };
        let Some(deleted) = parts.next() else {
            continue;
        };
        let Some(path) = parts.next() else {
            continue;
        };
        diffstat.files.insert(path.to_owned());
        if let Ok(value) = added.parse::<i64>() {
            diffstat.insertions = diffstat.insertions.saturating_add(value);
        }
        if let Ok(value) = deleted.parse::<i64>() {
            diffstat.deletions = diffstat.deletions.saturating_add(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git(dir: &Path, args: &[&str]) {
        let output = git_output(dir, args).expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            command_message(&output)
        );
    }

    #[test]
    fn non_git_directory_returns_graceful_status() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let status = collect_git_status(tmp.path());
        assert_eq!(status.state, "non_git");
        assert_eq!(status.modified, 0);
        assert!(!status.message.is_empty());
    }

    #[test]
    fn dirty_repo_reports_counts_and_tracked_diffstat() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init"]);
        git(tmp.path(), &["config", "user.email", "ax@example.invalid"]);
        git(tmp.path(), &["config", "user.name", "ax"]);

        fs::write(tmp.path().join("modified.txt"), "one\n").expect("write modified");
        fs::write(tmp.path().join("deleted.txt"), "gone\n").expect("write deleted");
        git(tmp.path(), &["add", "modified.txt", "deleted.txt"]);
        git(tmp.path(), &["commit", "-m", "init"]);

        fs::write(tmp.path().join("modified.txt"), "one\ntwo\n").expect("modify file");
        fs::remove_file(tmp.path().join("deleted.txt")).expect("delete file");
        fs::write(tmp.path().join("added.txt"), "fresh\n").expect("write added");
        git(tmp.path(), &["add", "added.txt"]);
        fs::write(tmp.path().join("untracked.txt"), "scratch\n").expect("write untracked");

        let status = collect_git_status(tmp.path());
        assert_eq!(status.state, "dirty");
        assert_eq!(status.modified, 1);
        assert_eq!(status.added, 1);
        assert_eq!(status.deleted, 1);
        assert_eq!(status.untracked, 1);
        assert_eq!(status.files_changed, 3);
        assert_eq!(status.insertions, 2);
        assert_eq!(status.deletions, 1);
    }
}
