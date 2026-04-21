//! Semantic styling helpers for the watch TUI.
//!
//! Color is a secondary cue in this crate: rows and statuses still
//! carry text labels, symbols, bold/dim, or reverse-video selection so
//! `NO_COLOR`, low-color terminals, and monochrome captures remain
//! usable.

use ax_proto::types::{AgentStatus, TaskPriority, TaskStatus};
use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    Success,
    Warning,
    Danger,
}

const ACCENT: Color = Color::Cyan;
const SUCCESS: Color = Color::Green;
const WARNING: Color = Color::Yellow;
const DANGER: Color = Color::Red;
const MUTED: Color = Color::Gray;
const ACTIVE_INACTIVE: Color = Color::White;
const ENTITY: Color = Color::LightBlue;
const SENDER: Color = Color::LightBlue;
const TASK_ID: Color = Color::LightMagenta;
const TRAFFIC_UP: Color = Color::LightBlue;
const TRAFFIC_DOWN: Color = Color::LightMagenta;
const COST: Color = Color::LightYellow;
const INFO: Color = Color::LightCyan;

pub(crate) fn colors_enabled() -> bool {
    colors_enabled_for(
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var_os("AX_TUI_NO_COLOR").is_some(),
    )
}

const fn colors_enabled_for(no_color: bool, ax_tui_no_color: bool) -> bool {
    !no_color && !ax_tui_no_color
}

fn with_fg(style: Style, color: Color) -> Style {
    if colors_enabled() {
        style.fg(color)
    } else {
        style
    }
}

pub(crate) fn strong() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub(crate) fn muted() -> Style {
    with_fg(Style::default(), MUTED)
}

pub(crate) fn disabled() -> Style {
    with_fg(Style::default().add_modifier(Modifier::DIM), MUTED)
}

pub(crate) fn column_header() -> Style {
    with_fg(strong(), MUTED)
}

pub(crate) fn meta_label() -> Style {
    with_fg(Style::default().add_modifier(Modifier::DIM), MUTED)
}

pub(crate) fn accent() -> Style {
    with_fg(Style::default(), ACCENT)
}

pub(crate) fn accent_bold() -> Style {
    accent().add_modifier(Modifier::BOLD)
}

pub(crate) fn active_label(focused: bool) -> Style {
    if focused {
        accent_bold()
    } else {
        with_fg(strong(), ACTIVE_INACTIVE)
    }
}

pub(crate) fn selection(focused: bool) -> Style {
    let style = Style::default().add_modifier(Modifier::REVERSED);
    if focused {
        with_fg(style, ACCENT)
    } else {
        style
    }
}

pub(crate) fn focus_border(focused: bool) -> Style {
    if focused {
        accent()
    } else {
        Style::default()
    }
}

pub(crate) fn severity(kind: Severity) -> Style {
    let color = match kind {
        Severity::Success => SUCCESS,
        Severity::Warning => WARNING,
        Severity::Danger => DANGER,
    };
    with_fg(Style::default(), color)
}

pub(crate) fn severity_bold(kind: Severity) -> Style {
    severity(kind).add_modifier(Modifier::BOLD)
}

pub(crate) fn notice(error: bool) -> Style {
    if error {
        severity(Severity::Danger)
    } else {
        severity(Severity::Success)
    }
}

pub(crate) fn agent_status(status: &AgentStatus) -> Style {
    match status {
        AgentStatus::Online => severity(Severity::Success),
        AgentStatus::Offline => disabled(),
        AgentStatus::Disconnected => severity(Severity::Warning),
    }
}

pub(crate) fn workspace(depth: usize) -> Style {
    if depth == 0 {
        accent_bold()
    } else {
        with_fg(Style::default(), ENTITY)
    }
}

pub(crate) fn running() -> Style {
    accent_bold()
}

pub(crate) fn idle() -> Style {
    with_fg(Style::default(), INFO)
}

pub(crate) fn traffic_up() -> Style {
    with_fg(Style::default(), TRAFFIC_UP)
}

pub(crate) fn traffic_down() -> Style {
    with_fg(Style::default(), TRAFFIC_DOWN)
}

pub(crate) fn cost() -> Style {
    with_fg(Style::default(), COST)
}

pub(crate) fn info() -> Style {
    with_fg(Style::default(), INFO)
}

pub(crate) fn sender() -> Style {
    with_fg(strong(), SENDER)
}

pub(crate) fn timestamp() -> Style {
    muted()
}

pub(crate) fn task_id() -> Style {
    with_fg(Style::default(), TASK_ID)
}

pub(crate) fn assignee() -> Style {
    with_fg(Style::default(), ENTITY)
}

pub(crate) fn priority(priority: Option<&TaskPriority>) -> Style {
    match priority {
        Some(TaskPriority::Urgent) => severity_bold(Severity::Danger),
        Some(TaskPriority::High) => severity_bold(Severity::Warning),
        Some(TaskPriority::Low) => muted(),
        Some(TaskPriority::Normal) | None => Style::default(),
    }
}

pub(crate) fn task_status(status: &TaskStatus, stale: bool) -> Style {
    if stale && matches!(status, TaskStatus::Pending | TaskStatus::InProgress) {
        return severity_bold(Severity::Warning);
    }
    match status {
        TaskStatus::Pending => Style::default(),
        TaskStatus::InProgress => accent_bold(),
        TaskStatus::Blocked => severity_bold(Severity::Warning),
        TaskStatus::Completed => severity(Severity::Success),
        TaskStatus::Failed => severity_bold(Severity::Danger),
        TaskStatus::Cancelled => muted(),
    }
}

pub(crate) fn task_title(status: &TaskStatus) -> Style {
    match status {
        TaskStatus::Completed | TaskStatus::Cancelled => muted(),
        TaskStatus::Failed => severity(Severity::Danger),
        _ => Style::default(),
    }
}

pub(crate) fn git_state(state: &str) -> Style {
    match state.trim() {
        "clean" => severity(Severity::Success),
        "dirty" => severity(Severity::Warning),
        "error" => severity_bold(Severity::Danger),
        "inaccessible" => severity(Severity::Warning),
        "non_git" => muted(),
        _ => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_flags_disable_color() {
        assert!(colors_enabled_for(false, false));
        assert!(!colors_enabled_for(true, false));
        assert!(!colors_enabled_for(false, true));
        assert!(!colors_enabled_for(true, true));
    }
}
