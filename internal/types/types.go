package types

import "time"

type AgentStatus string

const (
	StatusOnline       AgentStatus = "online"
	StatusOffline      AgentStatus = "offline"
	StatusDisconnected AgentStatus = "disconnected"
)

type WorkspaceInfo struct {
	Name        string      `json:"name"`
	Dir         string      `json:"dir"`
	Description string      `json:"description,omitempty"`
	Status      AgentStatus `json:"status"`
	StatusText  string      `json:"status_text,omitempty"`
	ConnectedAt *time.Time  `json:"connected_at,omitempty"`
}

type Message struct {
	ID        string    `json:"id"`
	From      string    `json:"from"`
	To        string    `json:"to"`
	Content   string    `json:"content"`
	CreatedAt time.Time `json:"created_at"`
}
