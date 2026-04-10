package daemon

import (
	"encoding/json"
	"fmt"

	"github.com/ashon/amux/internal/types"
)

type MessageType string

const (
	MsgRegister       MessageType = "register"
	MsgUnregister     MessageType = "unregister"
	MsgSendMessage    MessageType = "send_message"
	MsgBroadcast      MessageType = "broadcast"
	MsgReadMessages   MessageType = "read_messages"
	MsgListWorkspaces MessageType = "list_workspaces"
	MsgSetStatus      MessageType = "set_status"
	MsgSetShared      MessageType = "set_shared"
	MsgGetShared      MessageType = "get_shared"
	MsgListShared     MessageType = "list_shared"
	MsgPushMessage    MessageType = "push_message"
	MsgResponse       MessageType = "response"
	MsgError          MessageType = "error"
)

// Envelope is the wire format for daemon <-> MCP server communication.
// Sent as newline-delimited JSON over Unix socket.
type Envelope struct {
	ID      string          `json:"id"`
	Type    MessageType     `json:"type"`
	Payload json.RawMessage `json:"payload"`
}

// Request payloads

type RegisterPayload struct {
	Workspace   string `json:"workspace"`
	Dir         string `json:"dir,omitempty"`
	Description string `json:"description,omitempty"`
}

type SendMessagePayload struct {
	To      string `json:"to"`
	Message string `json:"message"`
}

type BroadcastPayload struct {
	Message string `json:"message"`
}

type ReadMessagesPayload struct {
	Limit int    `json:"limit,omitempty"`
	From  string `json:"from,omitempty"`
}

type SetStatusPayload struct {
	Status string `json:"status"`
}

type SetSharedPayload struct {
	Key   string `json:"key"`
	Value string `json:"value"`
}

type GetSharedPayload struct {
	Key string `json:"key"`
}

// Response payloads

type ResponsePayload struct {
	Success bool            `json:"success"`
	Data    json.RawMessage `json:"data,omitempty"`
}

type ErrorPayload struct {
	Message string `json:"message"`
}

type ListWorkspacesResponse struct {
	Workspaces []types.WorkspaceInfo `json:"workspaces"`
}

type ReadMessagesResponse struct {
	Messages []types.Message `json:"messages"`
}

type GetSharedResponse struct {
	Key   string `json:"key"`
	Value string `json:"value"`
	Found bool   `json:"found"`
}

type ListSharedResponse struct {
	Values map[string]string `json:"values"`
}

// Helper functions

func NewEnvelope(id string, msgType MessageType, payload any) (*Envelope, error) {
	data, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("marshal payload: %w", err)
	}
	return &Envelope{
		ID:      id,
		Type:    msgType,
		Payload: data,
	}, nil
}

func (e *Envelope) DecodePayload(v any) error {
	return json.Unmarshal(e.Payload, v)
}

func NewResponseEnvelope(id string, data any) (*Envelope, error) {
	dataBytes, err := json.Marshal(data)
	if err != nil {
		return nil, fmt.Errorf("marshal response data: %w", err)
	}
	return NewEnvelope(id, MsgResponse, &ResponsePayload{
		Success: true,
		Data:    dataBytes,
	})
}

func NewErrorEnvelope(id string, message string) (*Envelope, error) {
	return NewEnvelope(id, MsgError, &ErrorPayload{Message: message})
}
