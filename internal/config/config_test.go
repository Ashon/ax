package config_test

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/ashon/amux/internal/config"
)

func TestLoadMergesChildrenRecursively(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "services", "invest")
	grandChildDir := filepath.Join(childDir, "monitoring")

	if err := os.MkdirAll(grandChildDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	writeConfig(t, filepath.Join(grandChildDir, "amux.yaml"), `
project: monitoring
workspaces:
  alerts:
    dir: .
    description: alerts agent
`)

	writeConfig(t, filepath.Join(childDir, "amux.yaml"), `
project: invest
children:
  mon:
    dir: ./monitoring
workspaces:
  research:
    dir: .
    description: research agent
`)

	rootConfigPath := filepath.Join(rootDir, "amux.yaml")
	writeConfig(t, rootConfigPath, `
project: root
children:
  invest:
    dir: ./services/invest
workspaces:
  main:
    dir: .
    description: root agent
`)

	cfg, err := config.Load(rootConfigPath)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if cfg.Project != "root" {
		t.Fatalf("expected project root, got %q", cfg.Project)
	}

	if _, ok := cfg.Workspaces["main"]; !ok {
		t.Fatalf("expected root workspace main to exist")
	}
	if _, ok := cfg.Workspaces["invest.research"]; !ok {
		t.Fatalf("expected child workspace invest.research to exist")
	}
	if _, ok := cfg.Workspaces["invest.mon.alerts"]; !ok {
		t.Fatalf("expected grandchild workspace invest.mon.alerts to exist")
	}

	if got := cfg.Workspaces["invest.research"].Dir; got != childDir {
		t.Fatalf("expected invest.research dir %q, got %q", childDir, got)
	}
	if got := cfg.Workspaces["invest.mon.alerts"].Dir; got != grandChildDir {
		t.Fatalf("expected invest.mon.alerts dir %q, got %q", grandChildDir, got)
	}
}

func TestLoadRejectsCyclicChildren(t *testing.T) {
	rootDir := t.TempDir()
	childDir := filepath.Join(rootDir, "child")

	if err := os.MkdirAll(childDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	rootConfigPath := filepath.Join(rootDir, "amux.yaml")
	writeConfig(t, rootConfigPath, `
children:
  child:
    dir: ./child
`)

	writeConfig(t, filepath.Join(childDir, "amux.yaml"), `
children:
  root:
    dir: ..
`)

	if _, err := config.Load(rootConfigPath); err == nil {
		t.Fatal("expected cycle error, got nil")
	}
}

func writeConfig(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}
