package daemon

import "regexp"

var taskIDPattern = regexp.MustCompile(`(?i)task id:\s*([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})`)

func extractTaskID(content string) string {
	m := taskIDPattern.FindStringSubmatch(content)
	if len(m) != 2 {
		return ""
	}
	return m[1]
}
