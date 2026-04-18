//! Usage-domain types ported from `internal/usage/usage.go` and
//! `internal/usage/trend.go`. These appear inside the daemon's
//! `UsageTrendsResponse` and on-disk trend snapshots.

use std::ops::{Add, Sub};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Four-dimension token aggregate mirroring Go's `usage.Tokens`. Implements
/// `Add` / `Sub` so downstream callers can express the same arithmetic the
/// Go code does (`a.Add(b)` → `a + b`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
}

impl Tokens {
    #[must_use]
    pub fn total(self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }
}

impl Add for Tokens {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self {
            input: self.input + o.input,
            output: self.output + o.output,
            cache_read: self.cache_read + o.cache_read,
            cache_creation: self.cache_creation + o.cache_creation,
        }
    }
}

impl Sub for Tokens {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self {
            input: self.input - o.input,
            output: self.output - o.output,
            cache_read: self.cache_read - o.cache_read,
            cache_creation: self.cache_creation - o.cache_creation,
        }
    }
}

/// Transcript-derived MCP overhead proxy signals.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MCPProxyMetrics {
    pub total: i64,
    pub prompt_tokens: i64,
    pub prompt_signals: i64,
    pub tool_use_tokens: i64,
    pub tool_use_turns: i64,
}

/// Cumulative usage grouped by model name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelTotals {
    pub model: String,
    pub turns: i64,
    pub totals: Tokens,
}

/// One fixed-width trend bucket.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageBucket {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub totals: Tokens,
    pub mcp_proxy: MCPProxyMetrics,
    pub turns: i64,
}

/// Per-agent trend row inside a workspace trend response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentTrend {
    pub agent: String,
    pub available: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub latest_session_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub latest_transcript_path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buckets: Vec<UsageBucket>,
    pub total: Tokens,
    pub mcp_proxy: MCPProxyMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<DateTime<Utc>>,
    pub latest_tokens: Tokens,
    pub latest_mcp_proxy: MCPProxyMetrics,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub latest_model: String,
}

/// Workspace-level historical usage view.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceTrend {
    pub workspace: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    pub available: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unavailable_reason: String,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub bucket_minutes: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buckets: Vec<UsageBucket>,
    pub total: Tokens,
    pub mcp_proxy: MCPProxyMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<DateTime<Utc>>,
    pub latest_tokens: Tokens,
    pub latest_mcp_proxy: MCPProxyMetrics,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub latest_model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentTrend>,
}
