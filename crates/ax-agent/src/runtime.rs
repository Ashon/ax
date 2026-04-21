//! Supported agent runtimes and their canonical instruction-file names.

use std::fmt;

/// The two first-class runtimes ax ships. Normalization logic treats any
/// unknown runtime name as its own string so custom commands keep working;
/// this enum only covers the built-ins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Runtime {
    Claude,
    Codex,
}

pub const SUPPORTED_RUNTIMES: [Runtime; 2] = [Runtime::Claude, Runtime::Codex];

impl Runtime {
    /// Wire / YAML identifier for this runtime.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Normalise a runtime name: empty / `claude` → `Claude`, `codex`
    /// → `Codex`, anything else is treated as an opaque custom
    /// runtime name (returned as `None`).
    #[must_use]
    pub fn normalize(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "" | "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    /// Filename the runtime expects to read agent instructions from,
    /// relative to the workspace dir.
    #[must_use]
    pub fn instruction_file(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE.md",
            Self::Codex => "AGENTS.md",
        }
    }
}

impl fmt::Display for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolve a runtime name (possibly mixed case, whitespace, empty) into
/// the instruction filename `<WORKSPACE>/{CLAUDE.md|AGENTS.md|...}`. For
/// custom runtimes the caller must bring their own mapping; we return
/// `None` when the runtime is unknown.
#[must_use]
pub fn instruction_file(name: &str) -> Option<&'static str> {
    Runtime::normalize(name).map(Runtime::instruction_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_canonical_and_variant_forms() {
        assert_eq!(Runtime::normalize("claude"), Some(Runtime::Claude));
        assert_eq!(Runtime::normalize("CLAUDE"), Some(Runtime::Claude));
        assert_eq!(Runtime::normalize("  Claude  "), Some(Runtime::Claude));
        assert_eq!(Runtime::normalize(""), Some(Runtime::Claude));
        assert_eq!(Runtime::normalize("codex"), Some(Runtime::Codex));
        assert_eq!(Runtime::normalize("Codex"), Some(Runtime::Codex));
    }

    #[test]
    fn normalize_returns_none_for_unknown_runtime_names() {
        assert_eq!(Runtime::normalize("gpt-5"), None);
        assert_eq!(Runtime::normalize("custom-agent"), None);
    }

    #[test]
    fn instruction_file_matches_runtime_convention() {
        assert_eq!(Runtime::Claude.instruction_file(), "CLAUDE.md");
        assert_eq!(Runtime::Codex.instruction_file(), "AGENTS.md");
    }

    #[test]
    fn top_level_instruction_file_handles_empty_and_unknown() {
        // Empty string normalises to Claude for the orchestrator's bootstrap path.
        assert_eq!(instruction_file(""), Some("CLAUDE.md"));
        assert_eq!(instruction_file("codex"), Some("AGENTS.md"));
        assert_eq!(instruction_file("not-a-runtime"), None);
    }

    #[test]
    fn as_str_roundtrips_through_normalize() {
        for runtime in SUPPORTED_RUNTIMES {
            let round_tripped = Runtime::normalize(runtime.as_str());
            assert_eq!(round_tripped, Some(runtime));
        }
    }

    #[test]
    fn display_matches_wire_identifier() {
        assert_eq!(format!("{}", Runtime::Claude), "claude");
        assert_eq!(format!("{}", Runtime::Codex), "codex");
    }
}
