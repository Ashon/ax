package cmd

import (
	"slices"
	"testing"
)

func TestNormalizeClaudePassthroughArgsSupportsResumeAlias(t *testing.T) {
	got := normalizeClaudePassthroughArgs([]string{"resume", "session-123"})
	want := []string{"--resume", "session-123"}
	if !slices.Equal(got, want) {
		t.Fatalf("expected %v, got %v", want, got)
	}
}

func TestNormalizeClaudePassthroughArgsSupportsContinueAlias(t *testing.T) {
	got := normalizeClaudePassthroughArgs([]string{"continue"})
	want := []string{"--continue"}
	if !slices.Equal(got, want) {
		t.Fatalf("expected %v, got %v", want, got)
	}
}

func TestNormalizeClaudePassthroughArgsPreservesOtherArgs(t *testing.T) {
	got := normalizeClaudePassthroughArgs([]string{"agents", "--help"})
	want := []string{"agents", "--help"}
	if !slices.Equal(got, want) {
		t.Fatalf("expected %v, got %v", want, got)
	}
}
