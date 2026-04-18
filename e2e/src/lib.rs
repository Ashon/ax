//! Cross-crate integration harness. Exposes helpers that the live
//! orchestration tests under `tests/orchestration_live.rs` compose
//! into end-to-end scenarios (build the CLI, start an isolated
//! daemon, spin up a sandboxed codex team, drive a prompt, wait for
//! a validation script to pass).
//!
//! Non-live cross-crate tests (daemon roundtrip, config safety caps)
//! do not depend on this module and keep running under the default
//! `cargo test` flow.

#![forbid(unsafe_code)]

pub mod harness;
