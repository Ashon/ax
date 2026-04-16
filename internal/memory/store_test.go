package memory

import (
	"testing"
)

func TestStoreRememberPersistsAndRecallsActiveEntries(t *testing.T) {
	store := NewStore(t.TempDir())

	entry, err := store.Remember(ProjectScope("alpha"), "decision", "Routing", "Use the shared gateway for auth.", []string{"Auth", "gateway"}, "orchestrator", nil)
	if err != nil {
		t.Fatalf("remember: %v", err)
	}
	if entry.Kind != "decision" {
		t.Fatalf("kind=%q, want decision", entry.Kind)
	}
	if len(entry.Tags) != 2 || entry.Tags[0] != "auth" || entry.Tags[1] != "gateway" {
		t.Fatalf("unexpected tags: %+v", entry.Tags)
	}

	reloaded := NewStore(t.TempDir())
	reloaded.filePath = store.filePath
	if err := reloaded.Load(); err != nil {
		t.Fatalf("load: %v", err)
	}

	memories, err := reloaded.List(Query{Scopes: []string{ProjectScope("alpha")}})
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(memories) != 1 {
		t.Fatalf("expected one memory, got %+v", memories)
	}
	if memories[0].ID != entry.ID || memories[0].Content != entry.Content {
		t.Fatalf("unexpected memory payload: %+v", memories[0])
	}
}

func TestStoreRememberSupersedesPriorMemory(t *testing.T) {
	store := NewStore(t.TempDir())

	oldEntry, err := store.Remember(ProjectScope(""), "constraint", "Shell", "Use zsh for repo commands.", []string{"shell"}, "orchestrator", nil)
	if err != nil {
		t.Fatalf("remember old: %v", err)
	}
	newEntry, err := store.Remember(ProjectScope(""), "constraint", "Shell", "Use bash only for CI images.", []string{"shell"}, "orchestrator", []string{oldEntry.ID})
	if err != nil {
		t.Fatalf("remember new: %v", err)
	}

	active, err := store.List(Query{Scopes: []string{ProjectScope("")}})
	if err != nil {
		t.Fatalf("list active: %v", err)
	}
	if len(active) != 1 || active[0].ID != newEntry.ID {
		t.Fatalf("expected only active superseding memory, got %+v", active)
	}

	all, err := store.List(Query{Scopes: []string{ProjectScope("")}, IncludeSuperseded: true, Limit: 10})
	if err != nil {
		t.Fatalf("list all: %v", err)
	}
	if len(all) != 2 {
		t.Fatalf("expected both memories, got %+v", all)
	}
	var foundOld bool
	for _, entry := range all {
		if entry.ID == oldEntry.ID {
			foundOld = true
			if entry.SupersededBy != newEntry.ID || entry.SupersededAt == nil {
				t.Fatalf("expected old memory to be superseded by %q, got %+v", newEntry.ID, entry)
			}
		}
	}
	if !foundOld {
		t.Fatalf("expected superseded entry %q in %+v", oldEntry.ID, all)
	}
}
