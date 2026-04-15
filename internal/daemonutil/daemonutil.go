package daemonutil

import (
	"fmt"
	"os"
	"path/filepath"
)

func ExpandSocketPath(path string) string {
	if len(path) > 0 && path[0] == '~' {
		home, _ := os.UserHomeDir()
		path = filepath.Join(home, path[1:])
	}
	return path
}

// WakePrompt returns the tmux nudge text used to tell an agent to process
// queued messages. Static messaging policy now lives in managed startup
// instructions, so runtime wake text only carries dispatch-specific hints.
func WakePrompt(sender string, fresh bool) string {
	base := fmt.Sprintf(
		"대기 중인 메시지가 있습니다. `read_messages`로 확인하고 요청된 작업을 수행해 주세요. 회신이 필요하면 `send_message(to=\"%s\")`로 결과를 보내주세요.",
		sender,
	)
	if !fresh {
		return base
	}
	return base + " 이번 dispatch는 fresh-context 시작이 요청된 task입니다. 메시지에 `Task ID:`가 있으면 먼저 `get_task`로 해당 task를 확인하고, 이전 대화 문맥을 이어받았다고 가정하지 말고 현재 메시지와 task 정보만으로 다시 시작해 주세요."
}
