//! Byte-level wire-format golden tests for the shared domain types.
//!
//! Fixtures capture the exact JSON each struct must serialize to; a
//! decode → re-encode round-trip must reproduce every golden file
//! byte-for-byte so the daemon's on-wire format stays stable.

use ax_proto::types::{LifecycleTarget, Memory, Message, Task, WorkspaceGitStatus, WorkspaceInfo};
use serde::{de::DeserializeOwned, Serialize};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn load(name: &str) -> String {
    let path = format!("{FIXTURE_DIR}/{name}.json");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
        .trim_end_matches('\n')
        .to_owned()
}

fn assert_roundtrip<T>(name: &str)
where
    T: Serialize + DeserializeOwned,
{
    let raw = load(name);
    let decoded: T = serde_json::from_str(&raw).unwrap_or_else(|e| panic!("decode {name}: {e}"));
    let reencoded = serde_json::to_string(&decoded).expect("encode");
    assert_eq!(reencoded, raw, "byte drift in {name}");
}

#[test]
fn workspace_info_matches_wire_golden() {
    assert_roundtrip::<WorkspaceInfo>("workspace_info");
}

#[test]
fn workspace_git_status_matches_wire_golden() {
    assert_roundtrip::<WorkspaceGitStatus>("workspace_git_status");
}

#[test]
fn message_matches_wire_golden() {
    assert_roundtrip::<Message>("message");
}

#[test]
fn lifecycle_target_matches_wire_golden() {
    assert_roundtrip::<LifecycleTarget>("lifecycle_target");
}

#[test]
fn task_full_matches_wire_golden() {
    assert_roundtrip::<Task>("task_full");
}

#[test]
fn task_minimal_matches_wire_golden() {
    assert_roundtrip::<Task>("task_minimal");
}

#[test]
fn memory_matches_wire_golden() {
    assert_roundtrip::<Memory>("memory");
}
