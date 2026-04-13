package usage

import "testing"

func TestProjectDirFromCwd(t *testing.T) {
	cases := map[string]string{
		"/Users/ashon/git/github/ashon/ax":                          "-Users-ashon-git-github-ashon-ax",
		"/Users/ashon/.ax/orchestrator":                             "-Users-ashon--ax-orchestrator",
		"/Users/ashon/git/github/ashon/ax/.ax/orchestrator-ax":      "-Users-ashon-git-github-ashon-ax--ax-orchestrator-ax",
		"/home/user/proj":                                           "-home-user-proj",
		"/a.b.c/d":                                                  "-a-b-c-d",
	}
	for in, want := range cases {
		if got := ProjectDirFromCwd(in); got != want {
			t.Errorf("ProjectDirFromCwd(%q) = %q, want %q", in, got, want)
		}
	}
}
