//! Named-key resolution for `tmux send-keys`.
//!
//! Any token not in the map is treated as literal text (sent via `-l`).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedKey {
    /// tmux send-keys recognises this as a named key; send as-is.
    Special(&'static str),
    /// Not a named key — send via `send-keys -l` as literal text.
    Literal(String),
}

/// Resolve a user-supplied key token. Returns the tmux send-keys token
/// for named keys, or the original string as [`ResolvedKey::Literal`] so
/// the caller knows to add `-l`.
#[must_use]
pub fn resolve_key_token(key: &str) -> ResolvedKey {
    match key {
        "Enter" | "Return" => ResolvedKey::Special("Enter"),
        "Escape" | "Esc" => ResolvedKey::Special("Escape"),
        "Tab" => ResolvedKey::Special("Tab"),
        "Space" => ResolvedKey::Special("Space"),
        "BSpace" | "Backspace" => ResolvedKey::Special("BSpace"),
        "Delete" | "DC" => ResolvedKey::Special("DC"),
        "Up" => ResolvedKey::Special("Up"),
        "Down" => ResolvedKey::Special("Down"),
        "Left" => ResolvedKey::Special("Left"),
        "Right" => ResolvedKey::Special("Right"),
        "Home" => ResolvedKey::Special("Home"),
        "End" => ResolvedKey::Special("End"),
        "PageUp" | "PPage" => ResolvedKey::Special("PPage"),
        "PageDown" | "NPage" => ResolvedKey::Special("NPage"),
        "Ctrl-C" | "C-c" => ResolvedKey::Special("C-c"),
        "Ctrl-D" | "C-d" => ResolvedKey::Special("C-d"),
        "Ctrl-U" | "C-u" => ResolvedKey::Special("C-u"),
        "Ctrl-L" | "C-l" => ResolvedKey::Special("C-l"),
        "Ctrl-A" | "C-a" => ResolvedKey::Special("C-a"),
        "Ctrl-Z" | "C-z" => ResolvedKey::Special("C-z"),
        "Ctrl-R" | "C-r" => ResolvedKey::Special("C-r"),
        "Ctrl-W" | "C-w" => ResolvedKey::Special("C-w"),
        "Ctrl-K" | "C-k" => ResolvedKey::Special("C-k"),
        "Ctrl-E" | "C-e" => ResolvedKey::Special("C-e"),
        "Ctrl-B" | "C-b" => ResolvedKey::Special("C-b"),
        "Ctrl-F" | "C-f" => ResolvedKey::Special("C-f"),
        "Ctrl-P" | "C-p" => ResolvedKey::Special("C-p"),
        "Ctrl-N" | "C-n" => ResolvedKey::Special("C-n"),
        other => ResolvedKey::Literal(other.to_owned()),
    }
}
