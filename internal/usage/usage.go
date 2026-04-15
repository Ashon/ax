package usage

import "time"

// Tokens captures the four numeric dimensions we aggregate per turn.
type Tokens struct {
	Input         int64 `json:"input"`
	Output        int64 `json:"output"`
	CacheRead     int64 `json:"cache_read"`
	CacheCreation int64 `json:"cache_creation"`
}

// Total returns the summed token count across all tracked dimensions.
func (t Tokens) Total() int64 {
	return t.Input + t.Output + t.CacheRead + t.CacheCreation
}

// Add returns the sum of two Tokens.
func (t Tokens) Add(o Tokens) Tokens {
	return Tokens{
		Input:         t.Input + o.Input,
		Output:        t.Output + o.Output,
		CacheRead:     t.CacheRead + o.CacheRead,
		CacheCreation: t.CacheCreation + o.CacheCreation,
	}
}

// MCPProxyMetrics captures transcript-derived MCP overhead proxy signals.
// PromptTokens is an estimate derived from MCP attachment text injected into
// the prompt; ToolUseTokens/Turns track assistant turns that invoked MCP tools.
type MCPProxyMetrics struct {
	Total         int64 `json:"total"`
	PromptTokens  int64 `json:"prompt_tokens"`
	PromptSignals int64 `json:"prompt_signals"`
	ToolUseTokens int64 `json:"tool_use_tokens"`
	ToolUseTurns  int64 `json:"tool_use_turns"`
}

// Add returns the sum of two MCP proxy metric values.
func (m MCPProxyMetrics) Add(o MCPProxyMetrics) MCPProxyMetrics {
	out := MCPProxyMetrics{
		PromptTokens:  m.PromptTokens + o.PromptTokens,
		PromptSignals: m.PromptSignals + o.PromptSignals,
		ToolUseTokens: m.ToolUseTokens + o.ToolUseTokens,
		ToolUseTurns:  m.ToolUseTurns + o.ToolUseTurns,
	}
	out.Total = out.PromptTokens + out.ToolUseTokens
	return out
}

// ModelTotals groups cumulative usage by model name.
type ModelTotals struct {
	Model  string `json:"model"`
	Turns  int64  `json:"turns"`
	Totals Tokens `json:"totals"`
}

// WorkspaceUsage is the public snapshot for a single workspace.
// The daemon collector surfaces this via MCP in stage 3.
type WorkspaceUsage struct {
	Workspace        string          `json:"workspace"`
	TranscriptPath   string          `json:"transcript_path,omitempty"`
	SessionID        string          `json:"session_id,omitempty"`
	SessionStart     *time.Time      `json:"session_start,omitempty"`
	LastActivity     *time.Time      `json:"last_activity,omitempty"`
	CumulativeTotals Tokens          `json:"cumulative_totals"`
	CumulativeMCP    MCPProxyMetrics `json:"cumulative_mcp_proxy"`
	ByModel          []ModelTotals   `json:"by_model,omitempty"`
	CurrentContext   Tokens          `json:"current_context"`
	CurrentMCP       MCPProxyMetrics `json:"current_mcp_proxy"`
	CurrentModel     string          `json:"current_model,omitempty"`
	Turns            int64           `json:"turns"`
	Available        bool            `json:"available"`
	Error            string          `json:"error,omitempty"`
}
