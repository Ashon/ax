//! Stream pane renderer — messages view.
//!
//! History is read directly from `message_history.jsonl` under the
//! daemon state dir. We keep the reader minimal (no mtime cache
//! yet) — the TUI only refreshes every 250ms so re-reading a small
//! JSONL file is cheap enough.

use std::path::{Path, PathBuf};

use ax_daemon::{expand_socket_path, HistoryEntry};

const HISTORY_FILE_NAME: &str = "message_history.jsonl";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SnapshotRead<T> {
    Loaded(T),
    Missing,
    Error(String),
}

/// Which stream the body pane is showing. Each variant owns both a
/// list renderer (top half of the body) and a detail renderer
/// (bottom half) so every tab follows the same master/detail shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamView {
    /// Workspace fleet. List = agent rows from the config tree +
    /// live sessions; detail = the selected agent's reconcile /
    /// tmux tail / activity summary.
    Agents,
    Messages,
    Tasks,
    Tokens,
    /// Live tmux capture of `App::streamed_workspace` (set via the
    /// agents quick-action). Kept in the regular tab strip so
    /// operators can flip between stream and messages without
    /// leaving either mode.
    Stream,
}

impl StreamView {
    pub(crate) fn tab_label(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Messages => "messages",
            Self::Tasks => "tasks",
            Self::Tokens => "tokens",
            Self::Stream => "stream",
        }
    }

    pub(crate) const ALL: [Self; 5] = [
        Self::Agents,
        Self::Messages,
        Self::Tasks,
        Self::Tokens,
        Self::Stream,
    ];

    pub(crate) const DEFAULT_TABS: [Self; 4] =
        [Self::Agents, Self::Messages, Self::Tasks, Self::Tokens];

    pub(crate) fn tabs(stream_pinned: bool) -> &'static [Self] {
        if stream_pinned {
            &Self::ALL
        } else {
            &Self::DEFAULT_TABS
        }
    }
}

/// Resolve the absolute path to the daemon's history file given
/// whatever the user passed for `--socket`.
pub(crate) fn history_file_path(socket_path: &Path) -> PathBuf {
    let expanded = expand_socket_path(&socket_path.display().to_string());
    expanded
        .parent()
        .map_or_else(|| PathBuf::from(HISTORY_FILE_NAME), Path::to_path_buf)
        .join(HISTORY_FILE_NAME)
}

/// Load the newest `max_entries` history rows. Missing file or
/// parse errors are treated as "no history".
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn read_history(path: &Path, max_entries: usize) -> Vec<HistoryEntry> {
    match read_history_snapshot(path, max_entries) {
        SnapshotRead::Loaded(entries) => entries,
        SnapshotRead::Missing | SnapshotRead::Error(_) => Vec::new(),
    }
}

pub(crate) fn read_history_snapshot(
    path: &Path,
    max_entries: usize,
) -> SnapshotRead<Vec<HistoryEntry>> {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SnapshotRead::Missing,
        Err(e) => {
            return SnapshotRead::Error(format!("history snapshot read {}: {e}", path.display()));
        }
    };
    let mut entries: Vec<HistoryEntry> = Vec::new();
    for (idx, line) in data.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<HistoryEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                return SnapshotRead::Error(format!(
                    "history snapshot parse {} line {}: {e}",
                    path.display(),
                    idx + 1
                ));
            }
        }
    }
    if entries.len() > max_entries {
        entries.drain(..entries.len() - max_entries);
    }
    SnapshotRead::Loaded(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use tempfile::TempDir;

    fn entry(ts: &str, from: &str, to: &str, content: &str) -> HistoryEntry {
        let dt = chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc);
        HistoryEntry {
            timestamp: dt,
            from: from.into(),
            to: to.into(),
            content: content.into(),
            task_id: String::new(),
        }
    }

    #[test]
    fn read_history_returns_empty_when_file_is_missing() {
        let tmp = TempDir::new().unwrap();
        let got = read_history(&tmp.path().join("no_such.jsonl"), 50);
        assert!(got.is_empty());
    }

    #[test]
    fn read_history_keeps_tail_when_file_exceeds_limit() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("h.jsonl");
        let mut body = String::new();
        for i in 0..10 {
            let e = entry(
                &Utc.timestamp_opt(1_700_000_000 + i, 0)
                    .unwrap()
                    .to_rfc3339(),
                "a",
                "b",
                &format!("msg {i}"),
            );
            body.push_str(&serde_json::to_string(&e).unwrap());
            body.push('\n');
        }
        std::fs::write(&path, body).unwrap();
        let got = read_history(&path, 3);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].content, "msg 7");
        assert_eq!(got[2].content, "msg 9");
    }

    #[test]
    fn read_history_snapshot_distinguishes_missing_and_malformed_files() {
        let tmp = TempDir::new().unwrap();
        assert!(matches!(
            read_history_snapshot(&tmp.path().join("missing.jsonl"), 50),
            SnapshotRead::Missing
        ));

        let path = tmp.path().join("history.jsonl");
        std::fs::write(&path, "{not json}\n").unwrap();
        let got = read_history_snapshot(&path, 50);
        assert!(matches!(got, SnapshotRead::Error(message) if message.contains("line 1")));
    }

    #[test]
    fn stream_view_tabs_hide_stream_until_pinned() {
        assert_eq!(
            StreamView::tabs(false),
            &[
                StreamView::Agents,
                StreamView::Messages,
                StreamView::Tasks,
                StreamView::Tokens,
            ]
        );
        assert_eq!(
            StreamView::tabs(true),
            &[
                StreamView::Agents,
                StreamView::Messages,
                StreamView::Tasks,
                StreamView::Tokens,
                StreamView::Stream,
            ]
        );
    }
}
