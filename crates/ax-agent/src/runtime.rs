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
