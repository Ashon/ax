package cmd

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	axconfig "github.com/ashon/ax/internal/config"
	"github.com/ashon/ax/internal/workspace"
)

func TestArtifactCommandsRejectAmbiguousConfigBeforeWritingArtifacts(t *testing.T) {
	rootDir := t.TempDir()
	firstDir := filepath.Join(rootDir, "first")
	secondDir := filepath.Join(rootDir, "second")

	writeTestConfig(t, filepath.Join(firstDir, ".ax", "config.yaml"), `
workspaces:
  first:
    dir: .
`)
	writeTestConfig(t, filepath.Join(secondDir, ".ax", "config.yaml"), `
workspaces:
  second:
    dir: .
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeTestConfig(t, rootConfigPath, `
children:
  first:
    dir: ./first
    prefix: team
  second:
    dir: ./second
    prefix: team
`)

	oldConfigPath := configPath
	oldSocketPath := socketPath
	t.Cleanup(func() {
		configPath = oldConfigPath
		socketPath = oldSocketPath
	})

	configPath = rootConfigPath
	socketPath = filepath.Join(rootDir, "daemon.sock")

	commands := []struct {
		name string
		run  func() error
	}{
		{name: "up", run: func() error { return upCmd.RunE(upCmd, nil) }},
	}

	for _, command := range commands {
		t.Run(command.name, func(t *testing.T) {
			err := command.run()
			if !errors.Is(err, axconfig.ErrDuplicateChildPrefix) {
				t.Fatalf("expected duplicate child prefix error, got %v", err)
			}
			assertArtifactsAbsent(t, rootDir, firstDir, secondDir)
		})
	}
}

func TestArtifactCommandsRejectBrokenChildConfigBeforeWritingArtifacts(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "broken")

	writeTestConfig(t, filepath.Join(childDir, ".ax", "config.yaml"), `
workspaces:
  main: [
`)

	rootConfigPath := filepath.Join(rootDir, ".ax", "config.yaml")
	writeTestConfig(t, rootConfigPath, `
children:
  broken:
    dir: ./broken
`)

	oldConfigPath := configPath
	oldSocketPath := socketPath
	t.Cleanup(func() {
		configPath = oldConfigPath
		socketPath = oldSocketPath
	})

	configPath = rootConfigPath
	socketPath = filepath.Join(rootDir, "daemon.sock")

	commands := []struct {
		name string
		run  func() error
	}{
		{name: "up", run: func() error { return upCmd.RunE(upCmd, nil) }},
	}

	for _, command := range commands {
		t.Run(command.name, func(t *testing.T) {
			err := command.run()
			if err == nil {
				t.Fatal("expected an error, got nil")
			}
			if !strings.Contains(err.Error(), `load child "broken"`) || !strings.Contains(err.Error(), "parse config") {
				t.Fatalf("expected child parse failure details, got %v", err)
			}
			assertArtifactsAbsent(t, rootDir, childDir)
		})
	}
}

func writeTestConfig(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir %s: %v", filepath.Dir(path), err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func assertArtifactsAbsent(t *testing.T, dirs ...string) {
	t.Helper()
	for _, dir := range dirs {
		artifact := filepath.Join(dir, workspace.MCPConfigFile)
		if _, statErr := os.Stat(artifact); !errors.Is(statErr, os.ErrNotExist) {
			t.Fatalf("expected no artifact at %s, stat err=%v", artifact, statErr)
		}
	}
}
