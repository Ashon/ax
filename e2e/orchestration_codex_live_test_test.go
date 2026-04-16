package e2e

import "testing"

func TestFilteredEnvDropsRequestedKeys(t *testing.T) {
	in := []string{
		"HOME=/tmp/home",
		"TMUX=/tmp/tmux-sock,123,0",
		"TMUX_PANE=%1",
		"PATH=/usr/bin:/bin",
		"SHELL=/bin/zsh",
	}

	got := filteredEnv(in, "TMUX", "TMUX_PANE")

	for _, entry := range got {
		if entry == "TMUX=/tmp/tmux-sock,123,0" || entry == "TMUX_PANE=%1" {
			t.Fatalf("filteredEnv left tmux state behind: %v", got)
		}
	}
	if len(got) != 3 {
		t.Fatalf("filteredEnv length = %d, want 3 (%v)", len(got), got)
	}
}

func TestParseLeakedHostSessionsFiltersSandboxPaths(t *testing.T) {
	out := "" +
		"ax-cli|/tmp/ax-e2e-123/p/cli\n" +
		"ax-core|/tmp/ax-e2e-123/p/core\n" +
		"ax-core|/tmp/ax-e2e-123/p/core\n" +
		"ax-orchestrator|/Users/ashon.lee/.ax/orchestrator\n"

	got := parseLeakedHostSessions(out, []string{"/tmp/ax-e2e-123"})

	if len(got) != 2 {
		t.Fatalf("parseLeakedHostSessions len = %d, want 2 (%v)", len(got), got)
	}
	if got[0] != "ax-cli" || got[1] != "ax-core" {
		t.Fatalf("parseLeakedHostSessions = %v, want [ax-cli ax-core]", got)
	}
}

func TestParseLeakedHostSessionsMatchesPrivateTmpAlias(t *testing.T) {
	out := "" +
		"ax-cli|/private/tmp/ax-e2e-123/p/cli\n" +
		"ax-core|/private/tmp/ax-e2e-123/p/core\n"

	got := parseLeakedHostSessions(out, sandboxPathPrefixes("/tmp/ax-e2e-123"))

	if len(got) != 2 {
		t.Fatalf("parseLeakedHostSessions len = %d, want 2 (%v)", len(got), got)
	}
}
