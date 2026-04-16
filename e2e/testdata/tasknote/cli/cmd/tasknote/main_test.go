package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"example.com/tasknote/core/tasknote"
)

func TestAddCommandWritesTasksFile(t *testing.T) {
	dir := t.TempDir()

	code, stdout, stderr := runInDir(t, dir, "add", "Write docs")
	if code != 0 {
		t.Fatalf("run(add) code = %d, stderr = %q", code, stderr)
	}
	if !strings.Contains(stdout, "added task 1") {
		t.Fatalf("stdout = %q, want add confirmation", stdout)
	}

	tasks := readTasksFile(t, filepath.Join(dir, "tasks.json"))
	if len(tasks) != 1 {
		t.Fatalf("len(tasks) = %d, want 1", len(tasks))
	}
	if tasks[0].ID != 1 || tasks[0].Title != "Write docs" || tasks[0].Done {
		t.Fatalf("unexpected task state: %+v", tasks[0])
	}
}

func TestListCommandPrintsCheckboxes(t *testing.T) {
	dir := t.TempDir()
	writeTasksFile(t, filepath.Join(dir, "tasks.json"), []tasknote.Task{
		{ID: 1, Title: "Write docs"},
		{ID: 2, Title: "Ship release", Done: true},
	})

	code, stdout, stderr := runInDir(t, dir, "list")
	if code != 0 {
		t.Fatalf("run(list) code = %d, stderr = %q", code, stderr)
	}
	if !strings.Contains(stdout, "1. [ ] Write docs") {
		t.Fatalf("stdout = %q, want pending task line", stdout)
	}
	if !strings.Contains(stdout, "2. [x] Ship release") {
		t.Fatalf("stdout = %q, want completed task line", stdout)
	}
}

func TestDoneCommandUpdatesExistingTask(t *testing.T) {
	dir := t.TempDir()
	writeTasksFile(t, filepath.Join(dir, "tasks.json"), []tasknote.Task{
		{ID: 1, Title: "Write docs"},
	})

	code, stdout, stderr := runInDir(t, dir, "done", "1")
	if code != 0 {
		t.Fatalf("run(done) code = %d, stderr = %q", code, stderr)
	}
	if !strings.Contains(stdout, "completed task 1") {
		t.Fatalf("stdout = %q, want completion confirmation", stdout)
	}

	tasks := readTasksFile(t, filepath.Join(dir, "tasks.json"))
	if len(tasks) != 1 || !tasks[0].Done {
		t.Fatalf("unexpected tasks after done: %+v", tasks)
	}
}

func TestExportMarkdownPrintsChecklist(t *testing.T) {
	dir := t.TempDir()
	writeTasksFile(t, filepath.Join(dir, "tasks.json"), []tasknote.Task{
		{ID: 1, Title: "Write docs"},
		{ID: 2, Title: "Ship release", Done: true},
	})

	code, stdout, stderr := runInDir(t, dir, "export-markdown")
	if code != 0 {
		t.Fatalf("run(export-markdown) code = %d, stderr = %q", code, stderr)
	}
	if !strings.Contains(stdout, "- [ ] Write docs") {
		t.Fatalf("stdout = %q, want pending markdown line", stdout)
	}
	if !strings.Contains(stdout, "- [x] Ship release") {
		t.Fatalf("stdout = %q, want completed markdown line", stdout)
	}
}

func runInDir(t *testing.T, dir string, args ...string) (int, string, string) {
	t.Helper()
	oldWD, err := os.Getwd()
	if err != nil {
		t.Fatalf("getwd: %v", err)
	}
	if err := os.Chdir(dir); err != nil {
		t.Fatalf("chdir(%s): %v", dir, err)
	}
	defer func() {
		_ = os.Chdir(oldWD)
	}()

	var stdout bytes.Buffer
	var stderr bytes.Buffer
	code := run(args, &stdout, &stderr)
	return code, stdout.String(), stderr.String()
}

func readTasksFile(t *testing.T, path string) []tasknote.Task {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read tasks file: %v", err)
	}
	var tasks []tasknote.Task
	if err := json.Unmarshal(data, &tasks); err != nil {
		t.Fatalf("unmarshal tasks file: %v", err)
	}
	return tasks
}

func writeTasksFile(t *testing.T, path string, tasks []tasknote.Task) {
	t.Helper()
	data, err := json.MarshalIndent(tasks, "", "  ")
	if err != nil {
		t.Fatalf("marshal tasks file: %v", err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatalf("write tasks file: %v", err)
	}
}
