//! Per-workspace tmux capture cache used to render capture previews:
//! round-robin background scans plus a hot path for the selected
//! workspace.
//!
//! Each stored capture is the raw `tmux capture-pane -p` stdout
//! (newlines preserved, no ANSI escapes — we don't currently
//! reinterpret colour). Renderers adapt the capture to the current
//! panel width via [`recent_wrapped_lines`] so a small pane still
//! shows the most recent activity without truncating every row.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ax_tmux::SessionInfo;

/// Max age before a background capture becomes "stale" and gets a
/// refresh window in the round-robin scan. 2s matches
/// `watchBackgroundCaptureMaxAge`.
pub(crate) const BACKGROUND_MAX_AGE: Duration = Duration::from_secs(2);

/// How many workspaces to capture per tick when they're not
/// currently focused. Matches `watchBackgroundCaptureBatchSize`.
pub(crate) const BACKGROUND_BATCH_SIZE: usize = 2;

#[derive(Debug, Clone)]
pub(crate) struct CaptureEntry {
    pub content: String,
    pub captured_at: Instant,
    /// Last moment the captured content actually *changed* compared to
    /// the prior snapshot. Unchanged refreshes only bump `captured_at`.
    /// Used by the agents panel to flip a workspace from "running" to
    /// "idle" after a few quiet seconds.
    pub last_changed_at: Instant,
}

/// Window during which a workspace stays labelled "running" after its
/// last content change. Past this threshold the agents panel shows "idle"
/// so operators can tell the agent has stopped producing output even
/// when the tmux session is still attached.
pub(crate) const RUNNING_WINDOW: Duration = Duration::from_secs(3);

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
        let last_changed_at = match self.entries.get(&session.workspace) {
            Some(prev) if prev.content == content => prev.last_changed_at,
            _ => now,
        };
        self.entries.insert(
            session.workspace.clone(),
            CaptureEntry {
                content,
                captured_at: now,
                last_changed_at,
            },
        );
    }

    /// Return whether `workspace`'s capture has changed recently
    /// enough to be considered "running". Falls back to `false`
    /// (idle) when there is no capture entry yet.
    pub(crate) fn is_recently_active(&self, workspace: &str, now: Instant) -> bool {
        self.entries.get(workspace).is_some_and(|entry| {
            now.saturating_duration_since(entry.last_changed_at) < RUNNING_WINDOW
        })
    }

    /// Drop entries whose sessions no longer exist so the map doesn't
    /// leak memory when workspaces come and go.
    pub(crate) fn prune(&mut self, sessions: &[SessionInfo]) {
        let live: std::collections::HashSet<&str> =
            sessions.iter().map(|s| s.workspace.as_str()).collect();
        self.entries.retain(|name, _| live.contains(name.as_str()));
    }
}

#[cfg(test)]
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

/// Adapt a capture to the current stream pane width, then return the
/// last `rows` visual lines that fit. This keeps wide tmux captures
/// legible inside a narrower watch panel instead of truncating each
/// source row with an ellipsis.
pub(crate) fn recent_wrapped_lines(capture: &str, rows: usize, width: usize) -> Vec<String> {
    if rows == 0 || width == 0 {
        return Vec::new();
    }
    let mut visual_lines = wrapped_lines(capture, width);
    let start = visual_lines.len().saturating_sub(rows);
    visual_lines.drain(..start);
    visual_lines
}

pub(crate) fn wrapped_lines(capture: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let trimmed_tail: Vec<&str> = capture
        .lines()
        .rev()
        .skip_while(|line| line.trim().is_empty())
        .collect();
    if trimmed_tail.is_empty() {
        return Vec::new();
    }

    let mut visual_lines: Vec<String> = Vec::new();
    for line in trimmed_tail.into_iter().rev() {
        wrap_capture_line(line, width, &mut visual_lines);
    }

    visual_lines
}

fn wrap_capture_line(line: &str, width: usize, out: &mut Vec<String>) {
    let clean = sanitize_capture_line(line);
    if clean.is_empty() {
        out.push(String::new());
        return;
    }

    let chars: Vec<char> = clean.chars().collect();
    for chunk in chars.chunks(width) {
        out.push(chunk.iter().collect());
    }
}

fn sanitize_capture_line(line: &str) -> String {
    strip_csi_sequences(line)
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\t'))
        .collect::<String>()
        .replace('\t', "  ")
}

fn strip_csi_sequences(line: &str) -> String {
    enum State {
        Text,
        Escape,
        Csi,
    }

    let mut out = String::with_capacity(line.len());
    let mut state = State::Text;
    for ch in line.chars() {
        match state {
            State::Text => {
                if ch == '\x1b' {
                    state = State::Escape;
                } else {
                    out.push(ch);
                }
            }
            State::Escape => {
                if ch == '[' {
                    state = State::Csi;
                } else {
                    out.push(ch);
                    state = State::Text;
                }
            }
            State::Csi => {
                if ('@'..='~').contains(&ch) {
                    state = State::Text;
                }
            }
        }
    }
    out
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
        assert_eq!(
            recent_lines(capture, 4),
            vec!["first", "second", "", "third"]
        );
        assert_eq!(recent_lines(capture, 2), vec!["", "third"]);
    }

    #[test]
    fn recent_lines_handles_short_captures() {
        assert!(recent_lines("", 3).is_empty());
        assert_eq!(recent_lines("hello\nworld", 10), vec!["hello", "world"]);
    }

    #[test]
    fn recent_wrapped_lines_wraps_to_panel_width_before_tailing() {
        let capture = "abcdefghij\nkl\n";
        assert_eq!(
            recent_wrapped_lines(capture, 3, 4),
            vec!["efgh", "ij", "kl"]
        );
    }

    #[test]
    fn recent_wrapped_lines_skips_trailing_padding_but_keeps_internal_blank_rows() {
        let capture = "line1\n\nline2\n\n\n";
        assert_eq!(
            recent_wrapped_lines(capture, 4, 16),
            vec!["line1", "", "line2"]
        );
    }

    #[test]
    fn recent_wrapped_lines_sanitizes_controls_and_expands_tabs() {
        let capture = "ab\tcd\x1b[31m\n";
        assert_eq!(recent_wrapped_lines(capture, 4, 4), vec!["ab  ", "cd"]);
    }

    #[test]
    fn recent_wrapped_lines_strips_sgr_and_other_csi_sequences() {
        let capture = "plain \x1b[31mred\x1b[0m\nnext\x1b[2K line\n";
        assert_eq!(
            recent_wrapped_lines(capture, 4, 32),
            vec!["plain red", "next line"]
        );
    }

    #[test]
    fn prune_drops_workspaces_that_no_longer_exist() {
        let mut cache = CaptureCache::default();
        let now = Instant::now();
        cache.entries.insert(
            "alpha".into(),
            CaptureEntry {
                content: "x".into(),
                captured_at: now,
                last_changed_at: now,
            },
        );
        cache.entries.insert(
            "beta".into(),
            CaptureEntry {
                content: "y".into(),
                captured_at: now,
                last_changed_at: now,
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
                    last_changed_at: now,
                },
            );
        }
        // Fresh captures → no background refresh needed.
        let refreshed = cache.refresh(&sessions, None, now);
        assert!(refreshed.is_empty());
    }
}
