package daemon

import (
	"fmt"

	"github.com/ashon/ax/internal/memory"
)

func (d *Daemon) handleRememberMemoryEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p RememberMemoryPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode remember_memory: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	entry, err := d.memoryStore.Remember(p.Scope, p.Kind, p.Subject, p.Content, p.Tags, workspace, p.Supersedes)
	if err != nil {
		return nil, err
	}
	d.registry.Touch(workspace)
	return NewResponseEnvelope(env.ID, &MemoryResponse{Memory: *entry})
}

func (d *Daemon) handleRecallMemoriesEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p RecallMemoriesPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode recall_memories: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	memories, err := d.memoryStore.List(memory.Query{
		Scopes:            p.Scopes,
		Kind:              p.Kind,
		Tags:              p.Tags,
		IncludeSuperseded: p.IncludeSuperseded,
		Limit:             p.Limit,
	})
	if err != nil {
		return nil, err
	}
	d.registry.Touch(workspace)
	return NewResponseEnvelope(env.ID, &RecallMemoriesResponse{Memories: memories})
}
