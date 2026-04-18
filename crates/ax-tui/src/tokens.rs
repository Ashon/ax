//! Per-agent token extraction + tokens stream view. Mirrors the
//! subset of `cmd/watch_streams.go::parseAgentTokens` + the live
//! token table in `liveTokenLines` that we render without the
//! trend/MCP columns (those come in a later slice once we port the
//! usage-trend feed).
//!
//! Tokens are parsed directly from the tail of each workspace's
//! tmux capture. Claude CLI renders lines like
//! `  ✓ Completed (↑ 12.3k tokens ↓ 45.6k tokens · $0.42)` that
//! our regex scrapes into a compact [`AgentTokens`].

use std::sync::OnceLock;

use regex::Regex;

/// Recent-line window size we scan for token markers. Matches Go's
/// `parseAgentTokens` (`len(lines) - 30`).
const TOKEN_SCAN_WINDOW: usize = 30;

/// Single-workspace extraction result. Empty fields stay empty
/// strings so callers can treat "unknown" and "zero" differently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentTokens {
    pub workspace: String,
    pub up: String,
    pub down: String,
    pub total: String,
    pub cost: String,
}

impl AgentTokens {
    /// True when parsing extracted nothing useful. Used by the
    /// renderer to hide rows that would be all `-`.
    pub(crate) fn is_empty(&self) -> bool {
        self.up.is_empty() && self.down.is_empty() && self.total.is_empty() && self.cost.is_empty()
    }
}

fn token_up_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"↑\s*([\d.]+[kKmM]?)\s*tokens").unwrap())
}

fn token_down_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"↓\s*([\d.]+[kKmM]?)\s*tokens").unwrap())
}

fn token_any_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([\d.]+[kKmM]?)\s*tokens").unwrap())
}

fn cost_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$[\d.]+").unwrap())
}

fn claude_done_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bDone \(").unwrap())
}

/// Port of Go `parseAgentTokens`. Scans the last
/// `TOKEN_SCAN_WINDOW` capture lines for directional token counts +
/// cost; falls back to the "Done (…N.Mk tokens…)" summary line for
/// a single total when no directional pair was found.
pub(crate) fn parse_agent_tokens(workspace: &str, capture: &str) -> AgentTokens {
    let lines: Vec<&str> = capture.lines().collect();
    let start = lines.len().saturating_sub(TOKEN_SCAN_WINDOW);

    let mut out = AgentTokens {
        workspace: workspace.to_owned(),
        ..AgentTokens::default()
    };
    for line in &lines[start..] {
        if let Some(caps) = token_up_re().captures(line) {
            caps.get(1)
                .map_or("", |m| m.as_str())
                .clone_into(&mut out.up);
        }
        if let Some(caps) = token_down_re().captures(line) {
            caps.get(1)
                .map_or("", |m| m.as_str())
                .clone_into(&mut out.down);
        }
        if let Some(m) = cost_re().find(line) {
            m.as_str().clone_into(&mut out.cost);
        }
    }

    if out.up.is_empty() && out.down.is_empty() {
        for line in lines[start..].iter().rev() {
            if let Some(total) = extract_done_line_total(line) {
                out.total = total;
                break;
            }
        }
    }
    out
}

/// Parse the last `N tokens` number out of a Claude "Done (…)"
/// completion line. Returns `None` when the line isn't a Done
/// summary.
pub(crate) fn extract_done_line_total(line: &str) -> Option<String> {
    if !claude_done_re().is_match(line) {
        return None;
    }
    token_any_re()
        .captures_iter(line)
        .last()
        .and_then(|cap| cap.get(1).map(|m| m.as_str().to_owned()))
}

/// Convert `"12.3k"` / `"1.2M"` / `"450"` to a float count.
/// Caller decides how to display the result — we use it for
/// highlighting the cost/high-spender row in the tokens table.
pub(crate) fn parse_token_value(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let trimmed = s.trim();
    let (body, multiplier) = if let Some(body) = trimmed.strip_suffix(['k', 'K']) {
        (body, 1_000.0)
    } else if let Some(body) = trimmed.strip_suffix(['m', 'M']) {
        (body, 1_000_000.0)
    } else {
        (trimmed, 1.0)
    };
    body.parse::<f64>().unwrap_or(0.0) * multiplier
}

pub(crate) fn parse_cost_value(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let body = s.strip_prefix('$').unwrap_or(s);
    body.parse::<f64>().unwrap_or(0.0)
}

pub(crate) fn format_token_count(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else {
        format!("{v:.0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_tokens_extracts_directional_counts_and_cost() {
        let capture = concat!(
            "some unrelated line\n",
            "⏺ claude status: ↑ 12.3k tokens ↓ 45.6k tokens · $1.23\n",
            "next line\n",
        );
        let t = parse_agent_tokens("alpha", capture);
        assert_eq!(t.up, "12.3k");
        assert_eq!(t.down, "45.6k");
        assert_eq!(t.cost, "$1.23");
        assert_eq!(t.total, "");
    }

    #[test]
    fn parse_agent_tokens_falls_back_to_done_line_total() {
        let capture = "⏺ Done (16 tool uses · 93.9k tokens · 59s)\n";
        let t = parse_agent_tokens("alpha", capture);
        assert_eq!(t.up, "");
        assert_eq!(t.down, "");
        assert_eq!(t.total, "93.9k");
    }

    #[test]
    fn parse_agent_tokens_empty_when_capture_has_no_markers() {
        let t = parse_agent_tokens("alpha", "nothing tokeny here\n");
        assert!(t.is_empty());
    }

    #[test]
    fn parse_token_value_handles_k_m_and_plain_digits() {
        assert!((parse_token_value("12.3k") - 12_300.0).abs() < 1e-6);
        assert!((parse_token_value("1.5M") - 1_500_000.0).abs() < 1e-6);
        assert!((parse_token_value("450") - 450.0).abs() < 1e-6);
        assert!(parse_token_value("").abs() < 1e-6);
    }

    #[test]
    fn parse_cost_value_strips_dollar_prefix() {
        assert!((parse_cost_value("$1.23") - 1.23).abs() < 1e-6);
        assert!((parse_cost_value("0.5") - 0.5).abs() < 1e-6);
        assert!(parse_cost_value("").abs() < 1e-6);
    }

    #[test]
    fn format_token_count_uses_k_m_suffixes() {
        assert_eq!(format_token_count(450.0), "450");
        assert_eq!(format_token_count(12_345.0), "12.3k");
        assert_eq!(format_token_count(1_500_000.0), "1.5M");
    }

    #[test]
    fn extract_done_line_total_picks_last_tokens_number() {
        let line = "⏺ Done (16 tool uses · 93.9k tokens · 59s)";
        assert_eq!(extract_done_line_total(line), Some("93.9k".into()));
        assert_eq!(extract_done_line_total("no done marker"), None);
    }
}
