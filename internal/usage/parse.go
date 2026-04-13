package usage

import (
	"encoding/json"
	"time"
)

// rawRecord captures only the transcript fields we need. Unknown fields
// are ignored by encoding/json by default, which makes the parser
// resilient to Claude Code format drift.
type rawRecord struct {
	Type      string      `json:"type"`
	Timestamp time.Time   `json:"timestamp"`
	SessionID string      `json:"sessionId"`
	Cwd       string      `json:"cwd"`
	Message   *rawMessage `json:"message"`
}

type rawMessage struct {
	Role  string    `json:"role"`
	Model string    `json:"model"`
	Usage *rawUsage `json:"usage"`
}

type rawUsage struct {
	Input         int64 `json:"input_tokens"`
	Output        int64 `json:"output_tokens"`
	CacheRead     int64 `json:"cache_read_input_tokens"`
	CacheCreation int64 `json:"cache_creation_input_tokens"`
}

// parsedRecord is the normalized form the Aggregator consumes. Lines
// without a usage payload still propagate SessionID / Cwd / Timestamp
// so session-tracking metadata stays current.
type parsedRecord struct {
	SessionID string
	Cwd       string
	Timestamp time.Time
	Model     string
	Tokens    Tokens
	HasUsage  bool
}

// parseLine decodes a single jsonl line into a parsedRecord. It returns
// an error only on malformed JSON; records with no usage payload yield
// parsedRecord{HasUsage: false, ...} and no error.
func parseLine(data []byte) (parsedRecord, error) {
	var r rawRecord
	if err := json.Unmarshal(data, &r); err != nil {
		return parsedRecord{}, err
	}
	p := parsedRecord{
		SessionID: r.SessionID,
		Cwd:       r.Cwd,
		Timestamp: r.Timestamp,
	}
	if r.Message != nil && r.Message.Usage != nil {
		p.Model = r.Message.Model
		p.Tokens = Tokens{
			Input:         r.Message.Usage.Input,
			Output:        r.Message.Usage.Output,
			CacheRead:     r.Message.Usage.CacheRead,
			CacheCreation: r.Message.Usage.CacheCreation,
		}
		p.HasUsage = true
	}
	return p, nil
}
