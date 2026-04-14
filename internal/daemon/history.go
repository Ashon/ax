package daemon

import (
	"bufio"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"sync"
	"time"
)

type HistoryEntry struct {
	Timestamp time.Time `json:"ts"`
	From      string    `json:"from"`
	To        string    `json:"to"`
	Content   string    `json:"content"`
}

type History struct {
	mu      sync.Mutex
	entries []HistoryEntry
	maxSize int
	path    string // file path for persistence
}

func NewHistory(stateDir string, maxSize int) *History {
	return &History{
		maxSize: maxSize,
		path:    filepath.Join(stateDir, "message_history.jsonl"),
	}
}

func (h *History) Load() error {
	h.mu.Lock()
	defer h.mu.Unlock()

	if h.path == "" {
		return nil
	}
	f, err := os.Open(h.path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	defer f.Close()

	h.entries = nil
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1024*1024), 1024*1024)
	for scanner.Scan() {
		var entry HistoryEntry
		if err := json.Unmarshal(scanner.Bytes(), &entry); err != nil {
			return err
		}
		h.entries = append(h.entries, entry)
		if len(h.entries) > h.maxSize {
			h.entries = h.entries[len(h.entries)-h.maxSize:]
		}
	}
	if err := scanner.Err(); err != nil && err != io.EOF {
		return err
	}
	return nil
}

func (h *History) Append(from, to, content string) {
	h.mu.Lock()
	defer h.mu.Unlock()

	entry := HistoryEntry{
		Timestamp: time.Now(),
		From:      from,
		To:        to,
		Content:   content,
	}
	h.entries = append(h.entries, entry)
	if len(h.entries) > h.maxSize {
		h.entries = h.entries[len(h.entries)-h.maxSize:]
	}

	// Append to file
	h.appendToFile(entry)
}

func (h *History) appendToFile(entry HistoryEntry) {
	f, err := os.OpenFile(h.path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	data, _ := json.Marshal(entry)
	f.Write(append(data, '\n'))
}

// Recent returns the last n entries
func (h *History) Recent(n int) []HistoryEntry {
	h.mu.Lock()
	defer h.mu.Unlock()

	if n <= 0 || len(h.entries) == 0 {
		return nil
	}
	if n > len(h.entries) {
		n = len(h.entries)
	}
	result := make([]HistoryEntry, n)
	copy(result, h.entries[len(h.entries)-n:])
	return result
}

func (h *History) RecentMatching(n int, match func(HistoryEntry) bool) []HistoryEntry {
	h.mu.Lock()
	defer h.mu.Unlock()

	if n <= 0 || len(h.entries) == 0 {
		return nil
	}

	result := make([]HistoryEntry, 0, n)
	for i := len(h.entries) - 1; i >= 0; i-- {
		entry := h.entries[i]
		if !match(entry) {
			continue
		}
		result = append(result, entry)
		if len(result) == n {
			break
		}
	}
	return result
}

// HistoryFilePath returns the path to the history file for external readers (watch)
func HistoryFilePath(socketPath string) string {
	return filepath.Join(filepath.Dir(ExpandSocketPath(socketPath)), "message_history.jsonl")
}
