//! Claude transcript JSONL line parser.
//!
//! Each transcript line decodes to a [`ParsedRecord`] suitable for
//! feeding into the aggregator:
//! - Usage-bearing records carry the four-dimension [`Tokens`] breakdown
//!   and a request key used for per-turn deduplication.
//! - Non-usage records (user turns, attachments) still propagate
//!   `session_id` / `cwd` / `timestamp` so the aggregator can track
//!   session boundaries.
//!
//! MCP-proxy estimation: when a message uses an MCP tool, the turn's total
//! tokens count as MCP overhead. Attachment records of type
//! `mcp_instructions_delta` / `deferred_tools_delta` count their embedded
//! text at ~4 chars/token.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use ax_proto::usage::{MCPProxyMetrics, Tokens};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("decode transcript line: {0}")]
    Json(#[from] serde_json::Error),
}

/// Normalized parsed transcript line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedRecord {
    pub session_id: String,
    pub cwd: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub agent_id: String,
    pub request_id: String,
    pub message_id: String,
    pub model: String,
    pub tokens: Tokens,
    pub mcp_proxy: MCPProxyMetrics,
    pub has_usage: bool,
    /// Optional workspace hint extracted from `mcp_instructions_delta`
    /// attachments. Present only on the first record of a session.
    pub workspace_hint: String,
}

impl ParsedRecord {
    /// Key used by the aggregator to dedupe assistant requests: prefer
    /// `requestId`, fall back to `message.id`. Empty for non-usage lines.
    #[must_use]
    pub fn request_key(&self) -> String {
        if !self.request_id.is_empty() {
            format!("request:{}", self.request_id)
        } else if !self.message_id.is_empty() {
            format!("message:{}", self.message_id)
        } else {
            String::new()
        }
    }
}

/// Parse a single JSONL line. Malformed JSON returns an error; records
/// without usage still produce a valid `ParsedRecord` carrying session
/// metadata.
pub fn parse_line(data: &[u8]) -> Result<ParsedRecord, ParseError> {
    let raw: RawRecord = serde_json::from_slice(data)?;
    Ok(parsed_from_raw(raw))
}

// ---------- serde view of the on-disk record ----------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawRecord {
    #[serde(rename = "type")]
    _type: String,
    timestamp: Option<DateTime<Utc>>,
    #[serde(rename = "sessionId")]
    session_id: String,
    cwd: String,
    #[serde(rename = "agentId")]
    agent_id: String,
    #[serde(rename = "requestId")]
    request_id: String,
    attachment: Option<RawAttachment>,
    message: Option<RawMessage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawMessage {
    id: String,
    #[allow(dead_code)]
    role: String,
    model: String,
    usage: Option<RawUsage>,
    content: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_field_names)] // field names match the JSON keys Claude emits
struct RawUsage {
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawAttachment {
    #[serde(rename = "type")]
    att_type: String,
    #[serde(rename = "addedBlocks")]
    added_blocks: Vec<String>,
    #[serde(rename = "addedNames")]
    added_names: Vec<String>,
    #[serde(rename = "addedLines")]
    added_lines: Vec<String>,
    content: String,
}

fn parsed_from_raw(raw: RawRecord) -> ParsedRecord {
    let mut rec = ParsedRecord {
        session_id: raw.session_id,
        cwd: raw.cwd,
        timestamp: raw.timestamp,
        agent_id: raw.agent_id,
        request_id: raw.request_id,
        ..ParsedRecord::default()
    };

    rec.mcp_proxy = attachment_mcp_proxy(raw.attachment.as_ref());
    rec.workspace_hint = workspace_hint_from_attachment(raw.attachment.as_ref());

    if let Some(msg) = raw.message.as_ref() {
        if let Some(u) = &msg.usage {
            rec.message_id.clone_from(&msg.id);
            rec.model.clone_from(&msg.model);
            rec.tokens = Tokens {
                input: u.input_tokens,
                output: u.output_tokens,
                cache_read: u.cache_read_input_tokens,
                cache_creation: u.cache_creation_input_tokens,
            };
            rec.has_usage = true;
        }
        let extra = message_mcp_proxy(msg, rec.tokens, rec.has_usage);
        rec.mcp_proxy = add_mcp(rec.mcp_proxy, extra);
    }

    rec
}

// ---------- MCP proxy estimation ----------

fn attachment_mcp_proxy(att: Option<&RawAttachment>) -> MCPProxyMetrics {
    let Some(att) = att else {
        return MCPProxyMetrics::default();
    };
    match att.att_type.as_str() {
        "mcp_instructions_delta" => {
            let text = join_non_empty(&att.added_blocks, "\n");
            let text = if text.trim().is_empty() {
                att.content.trim().to_owned()
            } else {
                text
            };
            if text.is_empty() {
                return MCPProxyMetrics::default();
            }
            let est = estimate_proxy_tokens(&text);
            MCPProxyMetrics {
                total: est,
                prompt_tokens: est,
                prompt_signals: 1,
                ..MCPProxyMetrics::default()
            }
        }
        "deferred_tools_delta" => {
            let lines: Vec<&str> = if att.added_lines.is_empty() {
                att.added_names.iter().map(String::as_str).collect()
            } else {
                att.added_lines.iter().map(String::as_str).collect()
            };
            let filtered: Vec<&str> = lines
                .into_iter()
                .map(str::trim)
                .filter(|l| is_mcp_tool_reference(l))
                .collect();
            if filtered.is_empty() {
                return MCPProxyMetrics::default();
            }
            let text = filtered.join("\n");
            let est = estimate_proxy_tokens(&text);
            MCPProxyMetrics {
                total: est,
                prompt_tokens: est,
                prompt_signals: 1,
                ..MCPProxyMetrics::default()
            }
        }
        _ => MCPProxyMetrics::default(),
    }
}

fn message_mcp_proxy(msg: &RawMessage, tokens: Tokens, has_usage: bool) -> MCPProxyMetrics {
    if !has_usage {
        return MCPProxyMetrics::default();
    }
    let Some(content) = msg.content.as_ref() else {
        return MCPProxyMetrics::default();
    };
    if !message_uses_mcp_tool(content) {
        return MCPProxyMetrics::default();
    }
    let total = tokens.total();
    MCPProxyMetrics {
        total,
        tool_use_tokens: total,
        tool_use_turns: 1,
        ..MCPProxyMetrics::default()
    }
}

fn message_uses_mcp_tool(content: &serde_json::Value) -> bool {
    let Some(blocks) = content.as_array() else {
        return false;
    };
    for block in blocks {
        let Some(obj) = block.as_object() else {
            continue;
        };
        let block_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if block_type != "tool_use" {
            continue;
        }
        let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if is_mcp_tool_reference(name) {
            return true;
        }
        if name == "ToolSearch" {
            if let Some(query) = obj
                .get("input")
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("query"))
            {
                let text = query.to_string();
                if text.contains("mcp__")
                    || text.contains("ListMcpResourcesTool")
                    || text.contains("ReadMcpResourceTool")
                {
                    return true;
                }
            }
        }
    }
    false
}

fn is_mcp_tool_reference(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.starts_with("mcp__")
        || trimmed == "ListMcpResourcesTool"
        || trimmed == "ReadMcpResourceTool"
}

fn estimate_proxy_tokens(text: &str) -> i64 {
    let n = text.trim().chars().count();
    if n == 0 {
        0
    } else {
        n.div_ceil(4) as i64
    }
}

fn workspace_hint_from_attachment(att: Option<&RawAttachment>) -> String {
    let Some(att) = att else { return String::new() };
    if att.att_type != "mcp_instructions_delta" {
        return String::new();
    }
    for block in &att.added_blocks {
        if let Some(name) = extract_workspace_hint(block) {
            return name.trim().to_owned();
        }
    }
    String::new()
}

/// Match the workspace hint string the orchestrator injects into each
/// workspace prompt: `You are the "<name>" workspace agent in an ax
/// multi-agent environment.`
fn extract_workspace_hint(text: &str) -> Option<&str> {
    const PREFIX: &str = "You are the \"";
    const SUFFIX: &str = "\" workspace agent in an ax multi-agent environment.";
    let (_before, after_prefix) = text.split_once(PREFIX)?;
    let (name, _rest) = after_prefix.split_once(SUFFIX)?;
    Some(name)
}

fn add_mcp(a: MCPProxyMetrics, b: MCPProxyMetrics) -> MCPProxyMetrics {
    MCPProxyMetrics {
        total: a.total + b.total,
        prompt_tokens: a.prompt_tokens + b.prompt_tokens,
        prompt_signals: a.prompt_signals + b.prompt_signals,
        tool_use_tokens: a.tool_use_tokens + b.tool_use_tokens,
        tool_use_turns: a.tool_use_turns + b.tool_use_turns,
    }
}

fn join_non_empty(parts: &[String], sep: &str) -> String {
    parts
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(sep)
}
