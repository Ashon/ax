//! Runtime identity and bootstrap-path helpers.
//!
//! Library-layer port of `internal/agent`: the pieces callers reach for
//! that *don't* spawn processes. `Runtime` names + instruction-file
//! mapping, the Claude per-project state directory, the ax-managed
//! `CODEX_HOME` layout, and runtime launch helpers for ax-managed
//! sessions.

#![forbid(unsafe_code)]

mod claude;
mod codex;
mod launch;
mod runtime;
mod shell;

pub use claude::{claude_project_path, ClaudeProjectError};
pub use codex::{
    codex_home_key, codex_home_path, is_managed_codex_home, prepare_codex_home,
    prepare_codex_home_for_launch, remove_codex_home, CodexHomeError,
};
pub use launch::{run_in_dir_with_options, run_with_options, LaunchError, LaunchOptions};
pub use runtime::{instruction_file, Runtime, SUPPORTED_RUNTIMES};
pub use shell::shell_quote;
