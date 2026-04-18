//! Small helpers shared between the daemon's control loops. Today
//! this just holds [`wake_prompt`]; additional cross-module utilities
//! can land here without polluting the handler/scheduler modules.

/// Build the tmux nudge text used by the wake scheduler and direct
/// dispatch paths.
#[must_use]
pub fn wake_prompt(sender: &str, fresh: bool) -> String {
    let base = format!(
        "대기 중인 메시지가 있습니다. `read_messages`로 확인하고 요청된 작업을 수행해 주세요. 회신이 필요하면 `send_message(to=\"{sender}\")`로 결과를 보내주세요."
    );
    if !fresh {
        return base;
    }
    base + " 이번 dispatch는 fresh-context 시작이 요청된 task입니다. 메시지에 `Task ID:`가 있으면 먼저 `get_task`로 해당 task를 확인하고, 이전 대화 문맥을 이어받았다고 가정하지 말고 현재 메시지와 task 정보만으로 다시 시작해 주세요."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_prompt_plain_for_non_fresh() {
        let out = wake_prompt("ax.orchestrator", false);
        assert!(out.contains("ax.orchestrator"));
        assert!(!out.contains("fresh-context"));
    }

    #[test]
    fn wake_prompt_appends_fresh_context_suffix() {
        let out = wake_prompt("ax.orchestrator", true);
        assert!(out.contains("fresh-context"));
        assert!(out.contains("get_task"));
    }
}
