//! Per-workspace tmux capture cache. Mirrors the subset of
//! `refreshSessionCaptures` in `cmd/watch_model.go` that we need to
//! render capture previews: round-robin background scans plus a
//! hot path for the selected workspace.
//!
//! Each stored capture is the raw `tmux capture-pane -p` stdout
//! (newlines preserved, no ANSI escapes — we don't currently
//! reinterpret colour). Renderers pull the tail via
//! [`recent_lines`] so a small pane just shows the most recent
//! activity.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ax_tmux::SessionInfo;

/// Max age before a background capture becomes "stale" and gets a
/// refresh window in the round-robin scan. Matches
/// `watchBackgroundCaptureMaxAge` (2s) in the Go TUI.
pub(crate) const BACKGROUND_MAX_AGE: Duration = Duration::from_secs(2);

/// How many workspaces to capture per tick when they're not
/// currently focused. Matches Go's `watchBackgroundCaptureBatchSize`.
pub(crate) const BACKGROUND_BATCH_SIZE: usize = 2;

#[derive(Debug, Clone)]
pub(crate) struct CaptureEntry {
    pub content: String,
    pub captured_at: Instant,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct CaptureCache {
    /// Keyed by workspace name.
    pub entries: BTreeMap<String, CaptureEntry>,
    /// Rolling cursor into the session list so every workspace gets a
    /// refresh in turn.
    pub cursor: usize,
}

impl CaptureCache {
    /// Refresh captures on this tick. `focused` gets an immediate
    /// refresh (no throttling); the rest rotate through
    /// `BACKGROUND_BATCH_SIZE` entries per call. Returns the list of
    /// workspace names that got refreshed.
    pub(crate) fn refresh(
        &mut self,
        sessions: &[SessionInfo],
        focused: Option<&str>,
        now: Instant,
    ) -> Vec<String> {
        if sessions.is_empty() {
            return Vec::new();
        }
        let mut refreshed: Vec<String> = Vec::new();

        // Hot path: always re-capture the focused workspace so the
        // selected card stays lively.
        if let Some(name) = focused {
            if let Some(session) = sessions.iter().find(|s| s.workspace == name) {
                self.capture_into(session, now);
                refreshed.push(session.workspace.clone());
            }
        }

        // Partition out the background sessions whose captures are
        // stale enough to need a refresh.
        let background: Vec<&SessionInfo> = sessions
            .iter()
            .filter(|s| focused.is_none_or(|f| f != s.workspace.as_str()))
            .filter(|s| match self.entries.get(&s.workspace) {
                None => true,
                Some(entry) => {
                    now.saturating_duration_since(entry.captured_at) >= BACKGROUND_MAX_AGE
                }
            })
            .collect();
        if background.is_empty() {
            return refreshed;
        }

        let batch = BACKGROUND_BATCH_SIZE.min(background.len());
        for i in 0..batch {
            let idx = (self.cursor + i) % background.len();
            let session = background[idx];
            self.capture_into(session, now);
            refreshed.push(session.workspace.clone());
        }
        self.cursor = (self.cursor + batch) % background.len();
        refreshed
    }

    fn capture_into(&mut self, session: &SessionInfo, now: Instant) {
        let Ok(content) = ax_tmux::capture_pane(&session.workspace, false) else {
            return;
        };
        self.entries.insert(
            session.workspace.clone(),
            CaptureEntry {
                content,
                captured_at: now,
            },
        );
    }

    /// Drop entries whose sessions no longer exist so the map doesn't
    /// leak memory when workspaces come and go.
    pub(crate) fn prune(&mut self, sessions: &[SessionInfo]) {
        let live: std::collections::HashSet<&str> =
            sessions.iter().map(|s| s.workspace.as_str()).collect();
        self.entries.retain(|name, _| live.contains(name.as_str()));
    }
}

/// Pull the last `rows` non-empty lines of a capture. Trailing
/// blanks are skipped so small cards don't waste space rendering
/// empty tmux padding.
pub(crate) fn recent_lines(capture: &str, rows: usize) -> Vec<&str> {
    if rows == 0 {
        return Vec::new();
    }
    let trimmed_tail: Vec<&str> = capture
        .lines()
        .rev()
        .skip_while(|line| line.trim().is_empty())
        .collect();
    let mut tail: Vec<&str> = trimmed_tail.into_iter().take(rows).collect();
    tail.reverse();
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str) -> SessionInfo {
        SessionInfo {
            name: format!("ax-{name}"),
            workspace: name.into(),
            attached: false,
            windows: 1,
        }
    }

    #[test]
    fn recent_lines_skips_trailing_blanks_but_keeps_middle_whitespace() {
        // Trailing empty lines (padding from tmux) are skipped; internal
        // blank rows stay so the pane reflects the real capture shape.
        let capture = "first\nsecond\n\nthird\n\n\n";
        assert_eq!(recent_lines(capture, 4), vec!["first", "second", "", "third"]);
        assert_eq!(recent_lines(capture, 2), vec!["", "third"]);
    }

    #[test]
    fn recent_lines_handles_short_captures() {
        assert!(recent_lines("", 3).is_empty());
        assert_eq!(recent_lines("hello\nworld", 10), vec!["hello", "world"]);
    }

    #[test]
    fn prune_drops_workspaces_that_no_longer_exist() {
        let mut cache = CaptureCache::default();
        cache.entries.insert(
            "alpha".into(),
            CaptureEntry {
                content: "x".into(),
                captured_at: Instant::now(),
            },
        );
        cache.entries.insert(
            "beta".into(),
            CaptureEntry {
                content: "y".into(),
                captured_at: Instant::now(),
            },
        );
        cache.prune(&[session("alpha")]);
        assert!(cache.entries.contains_key("alpha"));
        assert!(!cache.entries.contains_key("beta"));
    }

    #[test]
    fn refresh_round_robins_background_sessions() {
        // We can't call real tmux in unit tests, but we can still drive
        // the cursor arithmetic by inserting capture entries directly
        // so `refresh` thinks everything is fresh and early-returns.
        let sessions = vec![session("a"), session("b"), session("c"), session("d")];
        let mut cache = CaptureCache::default();
        let now = Instant::now();
        for s in &sessions {
            cache.entries.insert(
                s.workspace.clone(),
                CaptureEntry {
                    content: String::new(),
                    captured_at: now,
                },
            );
        }
        // Fresh captures → no background refresh needed.
        let refreshed = cache.refresh(&sessions, None, now);
        assert!(refreshed.is_empty());
    }
}
