package usage

import "time"

// Tokens captures the four numeric dimensions we aggregate per turn.
type Tokens struct {
	Input         int64 `json:"input"`
	Output        int64 `json:"output"`
	CacheRead     int64 `json:"cache_read"`
	CacheCreation int64 `json:"cache_creation"`
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

// ModelTotals groups cumulative usage by model name.
type ModelTotals struct {
	Model  string `json:"model"`
	Turns  int64  `json:"turns"`
	Totals Tokens `json:"totals"`
}

// WorkspaceUsage is the public snapshot for a single workspace.
// The daemon collector surfaces this via MCP in stage 3.
type WorkspaceUsage struct {
	Workspace        string        `json:"workspace"`
	TranscriptPath   string        `json:"transcript_path,omitempty"`
	SessionID        string        `json:"session_id,omitempty"`
	SessionStart     *time.Time    `json:"session_start,omitempty"`
	LastActivity     *time.Time    `json:"last_activity,omitempty"`
	CumulativeTotals Tokens        `json:"cumulative_totals"`
	ByModel          []ModelTotals `json:"by_model,omitempty"`
	CurrentContext   Tokens        `json:"current_context"`
	CurrentModel     string        `json:"current_model,omitempty"`
	Turns            int64         `json:"turns"`
	Available        bool          `json:"available"`
	Error            string        `json:"error,omitempty"`
}
