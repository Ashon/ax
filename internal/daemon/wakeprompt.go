package daemon

import "github.com/ashon/ax/internal/daemonutil"

func WakePrompt(sender string, fresh bool) string {
	return daemonutil.WakePrompt(sender, fresh)
}
