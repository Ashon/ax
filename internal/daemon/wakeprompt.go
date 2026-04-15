package daemon

import "fmt"

// WakePrompt returns the tmux nudge text used to tell an agent to process
// queued messages and report back without creating ACK/status-message loops.
func WakePrompt(sender string, fresh bool) string {
	base := fmt.Sprintf(
		"read_messages MCP 도구로 수신 메시지를 확인하고 요청된 작업을 수행해 줘. 결과는 send_message(to=\"%s\")로 보내줘. 단, 단순 ACK/수신 확인/감사/상태 핑만의 메시지는 보내지 말고, 새 작업 결과나 필요한 정보가 있을 때만 회신해 줘. 이전과 실질적으로 동일한 메시지이거나, 지금 보내려는 답변이 이전 응답과 실질적으로 동일하면 회신하지 마세요. repeated summary/repeated confirmation도 억제 대상입니다. 진행 상태 공유가 필요하면 send_message 대신 set_status를 사용해 줘.%s",
		sender,
		taskEvidenceWakeContract(),
	)
	if !fresh {
		return base
	}
	return base + " 이번 dispatch는 fresh-context 시작이 요청된 task입니다. 메시지에 `Task ID:`가 있으면 먼저 `get_task`로 해당 task를 확인하고, `start_mode`가 `fresh`이면 이전 대화 문맥을 이어받았다고 가정하지 말고 현재 메시지와 task 정보만으로 다시 시작해 주세요."
}

func taskEvidenceWakeContract() string {
	return " 메시지에 `Task ID:`가 있으면, 전달되었거나 `read_messages`로 읽었다는 사실만으로 task를 claim한 것으로 간주하지 마세요. 먼저 `get_task`로 task 문맥을 확인한 뒤, 첫 task-flow action은 정확히 다음 4가지 중 하나여야 합니다: (1) `update_task(..., status=\"in_progress\", log=\"mode=implementation|inspection; scope=<exact files/modules>; validation=<plan>\")`, (2) exact blocker 또는 owner mismatch 보고, (3) superseded/invalid/fail 명시, (4) structured evidence와 함께 completion. owner mismatch나 missing dependency가 보이면 fail fast 하세요. substantive result를 이미 보낸 같은 `Task ID:`에 대해 concise current-status re-ask만 다시 오면 같은 요약을 반복하지 말고 새 delta가 있을 때만 회신하세요."
}
