package usage

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"
	"unicode/utf8"
)

// rawRecord captures only the transcript fields we need. Unknown fields
// are ignored by encoding/json by default, which makes the parser
// resilient to Claude Code format drift.
type rawRecord struct {
	Type       string         `json:"type"`
	Timestamp  time.Time      `json:"timestamp"`
	SessionID  string         `json:"sessionId"`
	Cwd        string         `json:"cwd"`
	AgentID    string         `json:"agentId"`
	Attachment *rawAttachment `json:"attachment"`
	Message    *rawMessage    `json:"message"`
}

type rawMessage struct {
	Role    string          `json:"role"`
	Model   string          `json:"model"`
	Usage   *rawUsage       `json:"usage"`
	Content json.RawMessage `json:"content"`
}

type rawUsage struct {
	Input         int64 `json:"input_tokens"`
	Output        int64 `json:"output_tokens"`
	CacheRead     int64 `json:"cache_read_input_tokens"`
	CacheCreation int64 `json:"cache_creation_input_tokens"`
}

type rawAttachment struct {
	Type        string   `json:"type"`
	AddedBlocks []string `json:"addedBlocks"`
	AddedNames  []string `json:"addedNames"`
	AddedLines  []string `json:"addedLines"`
	Content     string   `json:"content"`
}

type rawContentBlock struct {
	Type  string         `json:"type"`
	Name  string         `json:"name"`
	Input map[string]any `json:"input"`
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
	MCPProxy  MCPProxyMetrics
	HasUsage  bool
}

// parseLine decodes a single jsonl line into a parsedRecord. It returns
// an error only on malformed JSON; records with no usage payload yield
// parsedRecord{HasUsage: false, ...} and no error.
func parseLine(data []byte) (parsedRecord, error) {
	r, err := decodeRawRecord(data)
	if err != nil {
		return parsedRecord{}, err
	}
	return parsedRecordFromRaw(r), nil
}

func decodeRawRecord(data []byte) (rawRecord, error) {
	var r rawRecord
	if err := json.Unmarshal(data, &r); err != nil {
		return rawRecord{}, err
	}
	return r, nil
}

func parsedRecordFromRaw(r rawRecord) parsedRecord {
	p := parsedRecord{
		SessionID: r.SessionID,
		Cwd:       r.Cwd,
		Timestamp: r.Timestamp,
	}
	p.MCPProxy = attachmentMCPProxy(r.Attachment)
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
	if r.Message != nil {
		p.MCPProxy = p.MCPProxy.Add(messageMCPProxy(r.Message, p.Tokens, p.HasUsage))
	}
	return p
}

func attachmentMCPProxy(att *rawAttachment) MCPProxyMetrics {
	if att == nil {
		return MCPProxyMetrics{}
	}

	switch att.Type {
	case "mcp_instructions_delta":
		text := strings.TrimSpace(strings.Join(att.AddedBlocks, "\n"))
		if text == "" {
			text = strings.TrimSpace(att.Content)
		}
		if text == "" {
			return MCPProxyMetrics{}
		}
		return MCPProxyMetrics{
			Total:         estimateProxyTokens(text),
			PromptTokens:  estimateProxyTokens(text),
			PromptSignals: 1,
		}
	case "deferred_tools_delta":
		lines := att.AddedLines
		if len(lines) == 0 {
			lines = att.AddedNames
		}
		filtered := make([]string, 0, len(lines))
		for _, line := range lines {
			if isMCPToolReference(strings.TrimSpace(line)) {
				filtered = append(filtered, strings.TrimSpace(line))
			}
		}
		if len(filtered) == 0 {
			return MCPProxyMetrics{}
		}
		text := strings.Join(filtered, "\n")
		return MCPProxyMetrics{
			Total:         estimateProxyTokens(text),
			PromptTokens:  estimateProxyTokens(text),
			PromptSignals: 1,
		}
	default:
		return MCPProxyMetrics{}
	}
}

func messageMCPProxy(msg *rawMessage, tokens Tokens, hasUsage bool) MCPProxyMetrics {
	if msg == nil || len(msg.Content) == 0 || !hasUsage {
		return MCPProxyMetrics{}
	}
	if !messageUsesMCPTool(messageContentBlocks(msg.Content)) {
		return MCPProxyMetrics{}
	}
	total := tokens.Total()
	return MCPProxyMetrics{
		Total:         total,
		ToolUseTokens: total,
		ToolUseTurns:  1,
	}
}

func messageContentBlocks(raw json.RawMessage) []rawContentBlock {
	if len(raw) == 0 {
		return nil
	}
	var blocks []rawContentBlock
	if err := json.Unmarshal(raw, &blocks); err == nil {
		return blocks
	}
	return nil
}

func messageUsesMCPTool(blocks []rawContentBlock) bool {
	for _, block := range blocks {
		if block.Type != "tool_use" {
			continue
		}
		if isMCPToolReference(block.Name) {
			return true
		}
		if block.Name == "ToolSearch" && queryUsesMCPTool(block.Input) {
			return true
		}
	}
	return false
}

func queryUsesMCPTool(input map[string]any) bool {
	if input == nil {
		return false
	}
	query, ok := input["query"]
	if !ok {
		return false
	}
	text := fmt.Sprint(query)
	return strings.Contains(text, "mcp__") ||
		strings.Contains(text, "ListMcpResourcesTool") ||
		strings.Contains(text, "ReadMcpResourceTool")
}

func isMCPToolReference(name string) bool {
	name = strings.TrimSpace(name)
	if name == "" {
		return false
	}
	return strings.HasPrefix(name, "mcp__") ||
		name == "ListMcpResourcesTool" ||
		name == "ReadMcpResourceTool"
}

func estimateProxyTokens(text string) int64 {
	n := utf8.RuneCountInString(strings.TrimSpace(text))
	if n <= 0 {
		return 0
	}
	return int64((n + 3) / 4)
}
