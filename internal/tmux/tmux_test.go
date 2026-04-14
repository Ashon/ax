package tmux

import (
	"errors"
	"testing"
)

func TestResolveKeyToken(t *testing.T) {
	cases := []struct {
		in        string
		want      string
		isSpecial bool
	}{
		{"Enter", "Enter", true},
		{"Return", "Enter", true},
		{"Escape", "Escape", true},
		{"Esc", "Escape", true},
		{"BSpace", "BSpace", true},
		{"Backspace", "BSpace", true},
		{"Ctrl-C", "C-c", true},
		{"C-c", "C-c", true},
		{"Tab", "Tab", true},
		{"PageUp", "PPage", true},
		{"PageDown", "NPage", true},
		{"2", "2", false},
		{"hello", "hello", false},
		{"", "", false},
	}
	for _, tc := range cases {
		got, special := ResolveKeyToken(tc.in)
		if got != tc.want || special != tc.isSpecial {
			t.Errorf("ResolveKeyToken(%q) = (%q, %v), want (%q, %v)",
				tc.in, got, special, tc.want, tc.isSpecial)
		}
	}
}

func TestSendKeysMissingSession(t *testing.T) {
	err := SendKeys("ax-nonexistent-workspace-xyz", []string{"Enter"})
	if err == nil {
		t.Fatal("expected error for missing session, got nil")
	}
}

func TestEnvArgsSorted(t *testing.T) {
	got := envArgs(map[string]string{
		"ZETA":  "z",
		"ALPHA": "a",
	})
	want := []string{"-e", "ALPHA=a", "-e", "ZETA=z"}
	if len(got) != len(want) {
		t.Fatalf("envArgs length = %d, want %d (%v)", len(got), len(want), got)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("envArgs[%d] = %q, want %q (%v)", i, got[i], want[i], got)
		}
	}
}

func TestCommandWithEnvPrependsEnvBinary(t *testing.T) {
	got := commandWithEnv([]string{"ax", "run-agent"}, map[string]string{
		"B": "2",
		"A": "1",
	})
	want := []string{"env", "A=1", "B=2", "ax", "run-agent"}
	if len(got) != len(want) {
		t.Fatalf("commandWithEnv length = %d, want %d (%v)", len(got), len(want), got)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("commandWithEnv[%d] = %q, want %q (%v)", i, got[i], want[i], got)
		}
	}
}

func TestParseListSessionsResultParsesAxSessionsOnly(t *testing.T) {
	output := "ax-main 1 2\nother 0 4\nax-foo_bar 0 1\n"

	got, err := parseListSessionsResult(output, nil)
	if err != nil {
		t.Fatalf("parseListSessionsResult returned error: %v", err)
	}
	if len(got) != 2 {
		t.Fatalf("expected 2 ax sessions, got %d (%v)", len(got), got)
	}
	if got[0].Workspace != "main" || !got[0].Attached || got[0].Windows != 2 {
		t.Fatalf("unexpected first session: %+v", got[0])
	}
	if got[1].Workspace != "foo.bar" || got[1].Attached || got[1].Windows != 1 {
		t.Fatalf("unexpected second session: %+v", got[1])
	}
}

func TestParseListSessionsResultTreatsNoServerRunningAsEmpty(t *testing.T) {
	got, err := parseListSessionsResult("no server running on /tmp/tmux-123/default\n", errors.New("exit status 1"))
	if err != nil {
		t.Fatalf("expected no error, got %v", err)
	}
	if got != nil {
		t.Fatalf("expected nil sessions, got %v", got)
	}
}

func TestParseListSessionsResultReturnsUnexpectedTmuxErrors(t *testing.T) {
	_, err := parseListSessionsResult("", errors.New("exit status 1"))
	if err == nil {
		t.Fatal("expected error for unexpected tmux failure, got nil")
	}
}
