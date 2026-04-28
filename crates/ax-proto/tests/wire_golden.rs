//! Byte-level wire-format golden tests.
//!
//! Each fixture under `tests/fixtures/` is a frozen snapshot of the
//! daemon's on-wire JSON encoding for one envelope/payload variant.
//! For every fixture we:
//!
//!   1. deserialize into our typed envelope + payload,
//!   2. re-serialize from the typed form, and
//!   3. assert the re-serialized bytes exactly match the golden file.
//!
//! Any drift — key rename, `omitempty` mismatch, field reorder — will
//! make the byte comparison fail loudly. The daemon's on-wire format
//! is a stable contract; this test enforces it.

use ax_proto::{
    AgentLifecycleResponse, AgentStatusMetricsResponse, BroadcastPayload, BroadcastResponse,
    CancelTaskPayload, ControlLifecycleResponse, CreateTaskPayload, Envelope, ErrorPayload,
    GetAgentStatusMetricsPayload, InterveneTaskPayload, ListAgentStatusMetricsPayload,
    ListWorkspacesResponse, MessageType, RecordMcpToolActivityPayload, RegisterPayload,
    RememberMemoryPayload, ResponsePayload, SendMessagePayload, SendMessageResponse,
    StartTaskResponse, StatusResponse, UpdateAgentStatusMetricsPayload, UpdateTaskPayload,
    UsageTrendsResponse,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn load_fixture(name: &str) -> String {
    let path = format!("{FIXTURE_DIR}/{name}.json");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
        .trim_end_matches('\n')
        .to_owned()
}

/// Deserialize `raw` into `Envelope`, re-serialize, and assert byte match.
fn assert_envelope_roundtrip(name: &str, expected_type: &MessageType) -> Envelope {
    let raw = load_fixture(name);
    let env: Envelope = serde_json::from_str(&raw).expect("decode envelope");
    assert_eq!(&env.r#type, expected_type, "type mismatch in {name}");
    let out = serde_json::to_string(&env).expect("encode envelope");
    assert_eq!(out, raw, "envelope bytes differ for {name}");
    env
}

/// Decode the inner payload of a request envelope, re-encode, and assert the
/// bytes match the `payload` field of the original fixture. This catches
/// drift on a per-payload basis even if the outer envelope bytes happen to
/// match.
fn assert_payload_roundtrip<T>(name: &str)
where
    T: Serialize + DeserializeOwned,
{
    let raw = load_fixture(name);
    let v: Value = serde_json::from_str(&raw).expect("parse fixture as value");
    let payload_raw = serde_json::to_string(&v["payload"]).expect("encode payload value");
    let decoded: T = serde_json::from_str(&payload_raw).expect("decode payload");
    let reencoded = serde_json::to_string(&decoded).expect("encode payload");
    assert_eq!(reencoded, payload_raw, "payload bytes differ for {name}");
}

#[test]
fn register_request_matches_wire_golden() {
    assert_envelope_roundtrip("register", &MessageType::Register);
    assert_payload_roundtrip::<RegisterPayload>("register");
}

#[test]
fn register_minimal_skips_omitempty_fields() {
    assert_envelope_roundtrip("register_minimal", &MessageType::Register);
    assert_payload_roundtrip::<RegisterPayload>("register_minimal");
}

#[test]
fn send_message_with_config_path_matches_wire_golden() {
    assert_envelope_roundtrip("send_message", &MessageType::SendMessage);
    assert_payload_roundtrip::<SendMessagePayload>("send_message");
}

#[test]
fn send_message_without_config_path_omits_field() {
    assert_envelope_roundtrip("send_message_no_config", &MessageType::SendMessage);
    assert_payload_roundtrip::<SendMessagePayload>("send_message_no_config");
}

#[test]
fn record_mcp_tool_activity_matches_wire_golden() {
    assert_envelope_roundtrip(
        "record_mcp_tool_activity",
        &MessageType::RecordMcpToolActivity,
    );
    assert_payload_roundtrip::<RecordMcpToolActivityPayload>("record_mcp_tool_activity");
}

#[test]
fn update_agent_status_metrics_matches_wire_golden() {
    assert_envelope_roundtrip(
        "update_agent_status_metrics",
        &MessageType::UpdateAgentStatusMetrics,
    );
    assert_payload_roundtrip::<UpdateAgentStatusMetricsPayload>("update_agent_status_metrics");
}

#[test]
fn get_agent_status_metrics_matches_wire_golden() {
    assert_envelope_roundtrip(
        "get_agent_status_metrics",
        &MessageType::GetAgentStatusMetrics,
    );
    assert_payload_roundtrip::<GetAgentStatusMetricsPayload>("get_agent_status_metrics");
}

#[test]
fn list_agent_status_metrics_matches_wire_golden() {
    assert_envelope_roundtrip(
        "list_agent_status_metrics",
        &MessageType::ListAgentStatusMetrics,
    );
    assert_payload_roundtrip::<ListAgentStatusMetricsPayload>("list_agent_status_metrics");
}

#[test]
fn broadcast_matches_wire_golden() {
    assert_envelope_roundtrip("broadcast", &MessageType::Broadcast);
    assert_payload_roundtrip::<BroadcastPayload>("broadcast");
}

#[test]
fn status_response_matches_wire_golden() {
    let env = assert_envelope_roundtrip("resp_status", &MessageType::Response);
    let response: ResponsePayload = env.decode_payload().expect("decode response payload");
    let status: StatusResponse =
        serde_json::from_str(response.data.get()).expect("decode status response");
    assert_eq!(status.status, "registered");

    // Re-encode the inner StatusResponse and compare to the golden bytes.
    let encoded = serde_json::to_string(&status).unwrap();
    assert_eq!(encoded, response.data.get());
}

#[test]
fn send_message_response_sent_matches_wire_golden() {
    let env = assert_envelope_roundtrip("resp_send_message_sent", &MessageType::Response);
    let response: ResponsePayload = env.decode_payload().unwrap();
    let decoded: SendMessageResponse = serde_json::from_str(response.data.get()).unwrap();
    assert_eq!(decoded.message_id, "msg-1");
    assert_eq!(decoded.status, "sent");
    let reencoded = serde_json::to_string(&decoded).unwrap();
    assert_eq!(reencoded, response.data.get());
}

#[test]
fn send_message_response_suppressed_keeps_empty_message_id() {
    // `message_id` is always emitted on SendMessageResponse even when
    // empty — the round-trip must preserve the empty string rather
    // than dropping the key.
    let env = assert_envelope_roundtrip("resp_send_message_suppressed", &MessageType::Response);
    let response: ResponsePayload = env.decode_payload().unwrap();
    let decoded: SendMessageResponse = serde_json::from_str(response.data.get()).unwrap();
    assert_eq!(decoded.message_id, "");
    assert_eq!(decoded.status, "suppressed");
    let reencoded = serde_json::to_string(&decoded).unwrap();
    assert_eq!(reencoded, response.data.get());
}

#[test]
fn broadcast_response_matches_wire_golden() {
    let env = assert_envelope_roundtrip("resp_broadcast", &MessageType::Response);
    let response: ResponsePayload = env.decode_payload().unwrap();
    let decoded: BroadcastResponse = serde_json::from_str(response.data.get()).unwrap();
    assert_eq!(decoded.recipients, vec!["worker-a", "worker-b"]);
    assert_eq!(decoded.count, 2);
    let reencoded = serde_json::to_string(&decoded).unwrap();
    assert_eq!(reencoded, response.data.get());
}

// ---------- Expanded request payload coverage ----------

#[test]
fn create_task_payload_matches_wire_golden() {
    assert_envelope_roundtrip("create_task", &MessageType::CreateTask);
    assert_payload_roundtrip::<CreateTaskPayload>("create_task");
}

#[test]
fn update_task_payload_includes_optional_fields_only_when_present() {
    assert_envelope_roundtrip("update_task", &MessageType::UpdateTask);
    assert_payload_roundtrip::<UpdateTaskPayload>("update_task");
}

#[test]
fn cancel_task_payload_preserves_expected_version() {
    assert_envelope_roundtrip("cancel_task", &MessageType::CancelTask);
    assert_payload_roundtrip::<CancelTaskPayload>("cancel_task");
}

#[test]
fn intervene_task_payload_matches_wire_golden() {
    assert_envelope_roundtrip("intervene_task", &MessageType::InterveneTask);
    assert_payload_roundtrip::<InterveneTaskPayload>("intervene_task");
}

#[test]
fn remember_memory_uses_supersedes_ids_key() {
    // Critical: the wire field name is `supersedes_ids`, not `supersedes`.
    assert_envelope_roundtrip("remember_memory", &MessageType::RememberMemory);
    assert_payload_roundtrip::<RememberMemoryPayload>("remember_memory");
}

// ---------- Response coverage with embedded domain types ----------

fn assert_response_data_roundtrip<T>(fixture: &str)
where
    T: Serialize + DeserializeOwned,
{
    let raw = load_fixture(fixture);
    let env: Envelope = serde_json::from_str(&raw).unwrap();
    assert_eq!(env.r#type, MessageType::Response);
    let resp: ResponsePayload = env.decode_payload().unwrap();
    let decoded: T = serde_json::from_str(resp.data.get()).unwrap();
    let reencoded = serde_json::to_string(&decoded).unwrap();
    assert_eq!(
        reencoded,
        resp.data.get(),
        "response data drift in {fixture}"
    );
    // Full envelope also round-trips.
    let env_reencoded = serde_json::to_string(&env).unwrap();
    assert_eq!(env_reencoded, raw, "envelope drift in {fixture}");
}

#[test]
fn list_workspaces_response_matches_wire_golden() {
    assert_response_data_roundtrip::<ListWorkspacesResponse>("resp_list_workspaces");
}

#[test]
fn agent_status_metrics_response_matches_wire_golden() {
    assert_response_data_roundtrip::<AgentStatusMetricsResponse>("resp_agent_status_metrics");
}

#[test]
fn control_lifecycle_response_matches_wire_golden() {
    assert_response_data_roundtrip::<ControlLifecycleResponse>("resp_control_lifecycle");
}

#[test]
fn agent_lifecycle_response_matches_wire_golden() {
    assert_response_data_roundtrip::<AgentLifecycleResponse>("resp_agent_lifecycle");
}

#[test]
fn start_task_response_with_nested_task_matches_wire_golden() {
    assert_response_data_roundtrip::<StartTaskResponse>("resp_start_task");
}

#[test]
fn usage_trends_response_matches_wire_golden() {
    assert_response_data_roundtrip::<UsageTrendsResponse>("resp_usage_trends");
}

#[test]
fn error_envelope_round_trip() {
    // No fixture yet — synthesize an error envelope and verify the
    // struct round-trips through serde cleanly.
    let raw = r#"{"id":"err-1","type":"error","payload":{"message":"boom"}}"#;
    let env: Envelope = serde_json::from_str(raw).unwrap();
    assert_eq!(env.r#type, MessageType::Error);
    let err: ErrorPayload = env.decode_payload().unwrap();
    assert_eq!(err.message, "boom");
    assert_eq!(serde_json::to_string(&env).unwrap(), raw);
}
