package tasknote

import (
	"strings"
	"testing"
)

func TestAddTaskAssignsSequentialIDsAndTrimsTitle(t *testing.T) {
	initial := []Task{{ID: 1, Title: "Write docs"}}

	updated, added, err := AddTask(initial, "  Ship release  ")
	if err != nil {
		t.Fatalf("AddTask() error = %v", err)
	}
	if added.ID != 2 {
		t.Fatalf("added ID = %d, want 2", added.ID)
	}
	if added.Title != "Ship release" {
		t.Fatalf("added title = %q, want %q", added.Title, "Ship release")
	}
	if len(updated) != 2 {
		t.Fatalf("len(updated) = %d, want 2", len(updated))
	}
	if updated[1] != added {
		t.Fatalf("updated[1] = %+v, want %+v", updated[1], added)
	}
}

func TestCompleteTaskMarksRequestedTask(t *testing.T) {
	initial := []Task{
		{ID: 1, Title: "Write docs"},
		{ID: 2, Title: "Ship release"},
	}

	updated, err := CompleteTask(initial, 2)
	if err != nil {
		t.Fatalf("CompleteTask() error = %v", err)
	}
	if updated[0].Done {
		t.Fatal("expected first task to remain pending")
	}
	if !updated[1].Done {
		t.Fatal("expected second task to be marked done")
	}
}

func TestCompleteTaskRejectsMissingID(t *testing.T) {
	if _, err := CompleteTask([]Task{{ID: 1, Title: "Write docs"}}, 99); err == nil {
		t.Fatal("expected missing id to return an error")
	}
}

func TestRenderMarkdownUsesCheckboxes(t *testing.T) {
	markdown := RenderMarkdown([]Task{
		{ID: 1, Title: "Write docs"},
		{ID: 2, Title: "Ship release", Done: true},
	})

	if !strings.Contains(markdown, "- [ ] Write docs") {
		t.Fatalf("expected pending checklist entry, got %q", markdown)
	}
	if !strings.Contains(markdown, "- [x] Ship release") {
		t.Fatalf("expected completed checklist entry, got %q", markdown)
	}
}
