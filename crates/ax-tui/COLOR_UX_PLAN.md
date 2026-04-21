# ax-tui Color And Status UX Plan

This document records the current crate-local color and status-display rules for
the ratatui watch UI. It is intentionally scoped to `crates/ax-tui`; root
README or product docs should be handled by the docs owner.

## UX Principles

- Color is a secondary cue. Every important state also keeps text, symbols,
  reverse-video selection, bold/dim modifiers, or layout position.
- Default body text should stay readable. Secondary text uses a brighter
  gray-based muted style; disabled/offline values add `DIM` but still keep a
  textual label such as `offline`, `failed`, `blocked`, or `snapshot error`.
- Selection uses reverse video everywhere so the active row remains visible in
  color, low-color, and monochrome terminals.
- Semantic helpers in `src/theme.rs` own foreground colors. Render code should
  call helpers such as `theme::task_status`, `theme::agent_status`,
  `theme::traffic_up`, or `theme::sender` instead of setting ad hoc colors.
- Use the standard terminal palette rather than RGB values for compatibility
  across common terminal themes.

## Semantic Palette

- Focus/progress: cyan accent, usually bold for active tabs, focused titles,
  running sessions, and spinners.
- Success: green for online, completed, clean, and success notices.
- Warning: yellow for blocked, stale, dirty, disconnected, high-priority,
  destructive confirmation, and mixed git-state cases.
- Danger: red for snapshot errors, failed tasks, git errors, panic/failure text,
  and error notices.
- Muted: gray for timestamps, separators, column labels, placeholders, and
  metadata labels.
- Entity/value accents: light blue for workspaces/assignees/senders and
  traffic-up values, light magenta for task ids and traffic-down values,
  light yellow for cost, and light cyan for informational notes.

## No-Color Fallback

- `NO_COLOR` and `AX_TUI_NO_COLOR` disable foreground colors inside
  `src/theme.rs`.
- Fallback keeps bold, dim, and reverse-video modifiers. For example, selected
  rows remain reversed, urgent/failed values remain bold, and disabled values
  remain dim.
- Renderers must not add direct `.fg(...)` calls outside `theme.rs`; this keeps
  fallback behavior centralized.

## Agents Panel

- Header columns are rendered as separate spans: `NAME`, `STATE`, `UP`, `DOWN`,
  `COST`, and `INFO`.
- `NAME` carries the cursor, depth indentation, live marker, and workspace
  label. Project/root rows use the focus accent; child workspaces use the entity
  color; offline values use disabled styling.
- `STATE` distinguishes running, idle, online, offline, and disconnected using
  state-specific semantic helpers while preserving the text label.
- `UP`, `DOWN`, and `COST` use traffic/cost colors when values exist and muted
  style for `-`.
- `INFO` carries reconcile/status notes. Reconcile warnings such as
  `desired`/`runtime-only` use warning styling.
- Git status is displayed at group/project level, not repeated on every agent
  leaf row. A group row inspects direct child agent rows:
  - identical child git status: `git changed:N ?N`, `git clean`, etc. in
    `INFO`;
  - differing child git status: `git mixed` in warning style;
  - nested child groups roll up their own direct children so parent and child
    groups do not duplicate the same summary.
- Agent detail still shows the selected workspace's full git detail because the
  detail pane is scoped to one workspace.

## Tasks Panel

- The task list header and rows are split into `ID`, `STATE`, `OWNER`, and
  `TITLE` spans.
- `STATE` uses `theme::task_status`, including warning-bold stale active tasks,
  cyan running tasks, green completed tasks, red failed tasks, and muted
  cancelled tasks.
- `ID` uses task-id styling, `OWNER` uses assignee/entity styling, and `TITLE`
  remains default unless terminal task state should mute or warn it.
- Summary counters are independently styled: running, pending, stale, blocked,
  failed, done, queued message, divergence, high priority, and cancelled values
  each use their matching semantic helper.
- Task detail uses label/value spans for status, priority, assignee, updated,
  result, stale metadata, logs, and activity rows so operators can scan state
  without reading full lines.

## Messages Panel

- Message rows are split into timestamp, sender, recipient, optional short task
  id, separator, and body spans.
- Sender/recipient/task id colors make routing and task linkage scannable.
- Body text is lightly classified by content words: error/failure/panic maps to
  danger, blocked/blocker/warning/stale/wake maps to warning, and
  completed/success/done/`remaining owned dirty files=<none>` maps to success.
- Message detail reuses the same timestamp, sender, recipient, task id, and
  body semantics.

## Stream Formatter Cleanup

- `src/stream.rs` owns history loading and tab metadata only.
- The old `format_message_line` string formatter and its private
  `display_width`/`truncate` helpers were removed after message rendering moved
  into `src/render.rs` as span-based rows. This prevents release-build
  `dead_code` warnings.

## Current Verification

- Unit coverage includes viewport behavior, task summary/state formatting,
  git inline/detail formatting, group git roll-up, task row column spans,
  message row span splitting, and no-color flag behavior.
- Recent validation commands used for this implementation:
  - `cargo fmt -p ax-tui`
  - `cargo test -p ax-tui --lib`
  - `NO_COLOR=1 cargo test -p ax-tui --lib`
  - `cargo build --release --bin ax`
  - `git diff --check -- crates/ax-tui`
- Interactive screenshot/manual TUI verification is still not automated in this
  crate, so terminal-theme-specific visual perception remains a manual review
  risk.

## Follow-Up Options

- Add ratatui buffer snapshot tests for representative focused/unfocused,
  selected/unselected, group git roll-up, and no-color render states.
- Consider a small text-classification helper for message/task bodies if more
  status words need semantic styling.
- If product docs need user-facing explanation of color semantics, hand that
  off to the docs owner rather than expanding this crate-local plan.
