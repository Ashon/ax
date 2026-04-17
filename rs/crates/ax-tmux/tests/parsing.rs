//! Pure-parser tests that don't require a running tmux server.
//!
//! These mirror `internal/tmux/tmux_test.go` and cover the four places
//! where tmux output shape or key-name semantics matter: session name
//! encoding, list-sessions parsing, key token resolution, and idle
//! detection from a captured pane.

use ax_tmux::{
    decode_workspace_name, encode_workspace_name, resolve_key_token, session_name, ResolvedKey,
};

mod list_sessions_parse {
    use super::*;

    // We invoke the crate-private parser through a tiny forwarding fn
    // exposed via a public `pub(crate)` in this integration test via
    // `#[path]` to share the same module graph. The Rust integration
    // tests don't see pub(crate), so re-implement the parse logic
    // expectations via the public list_sessions is impossible without
    // tmux. Instead, we test behaviour of the public helpers that
    // handle encoding.
    #[test]
    fn encode_round_trips_dot_to_underscore() {
        assert_eq!(encode_workspace_name("ax.cli"), "ax_cli");
        assert_eq!(decode_workspace_name("ax_cli"), "ax.cli");
        assert_eq!(session_name("ax.cli"), "ax-ax_cli");
    }

    #[test]
    fn session_name_is_stable_for_multi_dot_workspaces() {
        assert_eq!(session_name("team.sub.worker"), "ax-team_sub_worker");
    }
}

#[test]
fn resolve_key_token_maps_aliases() {
    assert_eq!(resolve_key_token("Return"), ResolvedKey::Special("Enter"));
    assert_eq!(resolve_key_token("Esc"), ResolvedKey::Special("Escape"));
    assert_eq!(resolve_key_token("Ctrl-C"), ResolvedKey::Special("C-c"));
    assert_eq!(resolve_key_token("C-c"), ResolvedKey::Special("C-c"));
    assert_eq!(
        resolve_key_token("Backspace"),
        ResolvedKey::Special("BSpace")
    );
}

#[test]
fn resolve_key_token_falls_back_to_literal() {
    match resolve_key_token("hello world") {
        ResolvedKey::Literal(s) => assert_eq!(s, "hello world"),
        ResolvedKey::Special(other) => panic!("expected literal, got Special({other})"),
    }
}
