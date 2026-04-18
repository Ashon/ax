# Workspace Token / Context Usage Tracking — Design

Status: **design note (shipped, historical reference)**
Scope: ax daemon + MCP + watch UI
Owner: ax.usage (구현체는 `crates/ax-usage/`, `crates/ax-daemon/src/usage_trends.rs`,
`crates/ax-proto/src/usage.rs`에 살아 있습니다. 이 문서는 원래 Go로 초안된 것을
Rust 구현과 동기화한 스냅샷입니다.)

## 1. Goal

Track and expose per-workspace token consumption and current context usage
for every ax workspace that runs a Claude Code session, so that
orchestrators and users can see live cost/load per agent.

## 2. Data source

Claude Code persists each session as a newline-delimited JSON transcript
under `~/.claude/projects/<cwd-hash>/<sessionId>.jsonl`. The directory name
is the workspace `cwd` with `/` and `.` replaced by `-`, with a leading
`-` prefix.

### 2.1 cwd → project dir mapping

Verified empirically: `/Users/ashon/git/github/ashon/ax` maps to
`/Users/ashon/.claude/projects/-Users-ashon-git-github-ashon-ax`, and
`/Users/ashon/.ax/orchestrator` maps to
`/Users/ashon/.claude/projects/-Users-ashon--ax-orchestrator` (the
leading `.` collapses into a `-`, producing `--`).

Rule: replace every `/` and `.` in the absolute cwd with `-`. Leading slash
yields a leading `-`. This exactly reproduces every observed directory name.

### 2.2 Session file selection

Each `ax up` (or Claude Code relaunch) creates a new `<uuid>.jsonl` file
in the project dir. For a given workspace we want the **most recently
modified** transcript that belongs to the active tmux session.

Primary heuristic: pick the file whose `mtime` is the latest and whose
first record's `cwd` equals the workspace directory. This handles cwd
hash collisions (two different dirs can't share the same `-`-encoded name
in practice, but we verify `cwd` from the file body for safety).

Fallback: if no matching transcript exists, report `usage: unavailable`
rather than erroring — the workspace may have just launched or may not be
running a claude runtime at all (codex or custom).

### 2.3 Record shape (observed)

User messages: `{type: "user", message: {role, content}, ...}` — **no
usage field**.

Assistant messages: `{type: "assistant", message: {role, model, content,
usage}, ...}`. The `usage` object contains the fields we care about:

```json
{
  "input_tokens": 3,
  "cache_creation_input_tokens": 6448,
  "cache_read_input_tokens": 12253,
  "output_tokens": 295,
  "cache_creation": {
    "ephemeral_1h_input_tokens": 6448,
    "ephemeral_5m_input_tokens": 0
  },
  "service_tier": "standard",
  "iterations": [ ... ]
}
```

Other relevant top-level fields on the assistant record: `timestamp`
(ISO8601 UTC), `model` (from `message.model`), `sessionId`, `cwd`,
`gitBranch`. We capture `model` and `timestamp` per record.

Records without `message.usage` (system events, attachments,
file-history-snapshots, user turns) are skipped silently.

## 3. Data model

Crate: `ax-usage` — parser, tailer, and in-memory aggregation (wire types는
`ax-proto::usage`).

```rust
// crates/ax-proto/src/usage.rs
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
}

pub struct ModelTotals {
    pub model: String,
    pub turns: i64,
    pub totals: Tokens,
}

// 실제 snapshot은 crates/ax-usage/src/aggregator.rs의
// `UsageSnapshot`과 crates/ax-usage/src/history.rs의
// `WorkspaceHistory` / `CurrentSnapshot`로 구현되어 있으며
// 아래 필드 구성을 유지합니다:
//   workspace, transcript_path, session_id, session_start,
//   last_activity, cumulative_totals, by_model,
//   current_context, current_model, turns, available, error
```

### 3.1 "Current context" definition

Claude Code's `usage.input_tokens` is the **new** input tokens added in
that turn; `cache_read_input_tokens + cache_creation_input_tokens` is the
cached portion. The effective live context is therefore
`input + cache_read + cache_creation` on the most recent assistant record.
We expose this as `CurrentContext`, and clients can show a percentage
against the model's context window (stored in a static map keyed by
model name, or left to the UI).

### 3.2 Cumulative totals

Summed across **every** assistant record in the latest transcript file,
grouped by `model`. Rolled up to `CumulativeTotals` (all models) and
`ByModel`.

## 4. Collector

Crate: `ax-daemon` (wires to `ax-usage`). 실제 구현은 tick-기반이 아닌 on-demand
스캔으로 단순화되었습니다 — `crates/ax-daemon/src/usage_trends.rs`가
`handle_usage_trends_envelope`에서 `ax-usage::query_workspace_trends`를 호출해
transcript를 스캔하고 `ax-proto::UsageTrendsResponse`로 응답합니다.

```rust
// 개념적 형태 (tick-기반 draft)
struct UsageCollector {
    by_ws: HashMap<String, UsageSnapshot>,   // ax-usage::UsageSnapshot
    state: HashMap<String, FileState>,       // per-workspace tailing
}

struct FileState {
    path: PathBuf,
    offset: u64,                             // last parsed byte offset
    snapshot: UsageSnapshot,
}
```

- Draft plan called for a daemon background task on a 2s tick
  (configurable), iterating over `Registry::list()` to pick up
  workspaces currently connected. The shipped implementation scans on
  demand instead.
- For each workspace: resolve transcript path (via cwd in `WorkspaceInfo.Dir`),
  stat the file, if size grew then read from `offset` to `EOF` and parse
  new JSONL records. Update totals incrementally. No full re-parse.
- If the active transcript file changes (different `sessionId` from last
  tick — detected by inspecting the first record of the newest file), we
  reset `offset=0` and rebuild the aggregate for that workspace.
- Cached result stored in `byWS`. `Get(name)` returns a copy.

### 4.1 Why tail, not full re-parse

Transcripts grow to MB-size within minutes of activity (4.4MB for one
active session at the time of writing). Full re-parse on every tick would
be expensive. JSONL is append-only, and we only need the additional
records since last check, so tail is both simpler and cheaper.

### 4.2 Defensive parsing

- Each line parsed in isolation via `json.Unmarshal` into a minimal
  `struct{ Type string; Timestamp time.Time; Message struct{ Model
  string; Usage *rawUsage } }`.
- Unknown fields ignored (default `encoding/json` behavior).
- Missing `Usage` → skip record silently.
- Malformed line → count into `parseErrors` metric, skip.
- If `Usage` is present but missing numeric fields, treat absent fields
  as 0.
- Repeated assistant frames for the same Claude request are coalesced by
  `requestId` (falling back to `message.id`) so cumulative usage and MCP
  proxy totals track the latest request-level usage once, rather than
  summing every intermediate thinking/tool frame.

## 5. MCP exposure

### 5.1 Chosen: new tool `get_workspace_usage`

Rationale: `list_workspaces` is used frequently and the usage payload is
relatively large (and per-workspace cache-lookup rather than
registry-scanned). A dedicated tool keeps `list_workspaces` fast and gives
the client an explicit opt-in.

```
mcp.NewTool("get_workspace_usage",
    mcp.WithDescription("Return cumulative token counts, current context, and per-model totals for a workspace."),
    mcp.WithString("workspace", mcp.Required(), mcp.Description("Workspace name. Use 'all' to return a map of every workspace.")),
)
```

Handler calls `DaemonClient.GetWorkspaceUsage(name)` → daemon envelope
`MsgGetWorkspaceUsage` → `UsageCollector.Get(name)`.

Envelope added: `MsgGetWorkspaceUsage` with `GetWorkspaceUsagePayload{
Name string }` request and `GetWorkspaceUsageResponse{ Usage ... }`
response (or `Usages map[string]WorkspaceUsage` when `Name=="all"`).

### 5.2 Watch UI integration (proposal only for stage 1)

Add a right-aligned column `ctx` showing `CurrentContext` total as a
short humanized string (e.g. `184k`). Behind `--usage` flag initially to
avoid cluttering until stable. Sidebar row format:

```
● ax.daemon       opus-4-6     ctx 184k   turns 592
```

Not implemented in stage 2; saved for stage 3 after the collector is
proven.

### 5.3 CLI proposal (optional stage 3+)

`ax usage [workspace]` prints a table using `list_workspaces` +
`get_workspace_usage(all)`:

```
WORKSPACE        MODEL          CTX      IN     OUT    CACHE-R  CACHE-W  TURNS
ax.daemon        opus-4-6       184k    1.2k    77k     182k     6.4k    592
ax.orchestrator  opus-4-6       0       0       0       0        0       0
```

## 6. Edge cases

1. **Multiple transcript files** in the project dir (session restart,
   parallel claude run): pick the one with newest mtime whose first
   record matches the workspace cwd. Older ones are ignored — we don't
   merge across sessions to avoid confusing cumulative semantics.
2. **No transcript yet**: `Available=false`, `Error="no transcript"`.
3. **Non-claude runtime** (codex, custom shell): `Available=false`,
   `Error="runtime=<name> unsupported"`. UI shows `—`.
4. **Claude Code format drift**: defensive parser skips anything unknown;
   a `parseErrors` counter on the collector surfaces drift via stderr
   logs.
5. **Deleted/rotated transcript**: stat error → reset state, mark
   unavailable until next valid file appears.
6. **Huge transcripts**: tail-based incremental reads bound memory to
   `O(delta bytes since last tick)`.
7. **Workspace with no `Dir` registered**: the workspace was registered
   without `Dir`, so we can't resolve a project hash. `Available=false`.
   Daemon `Register` already stores `Dir` so most real workspaces have it.

### 6.1 Historical availability vs live presence

For history/trend responses, `WorkspaceTrend.Available` and
`AgentTrend.Available` mean "transcript history was attributable for the
requested binding/window", not "the workspace is currently online in the
daemon registry". Callers that care about live presence must join against
`list_workspaces`/session state separately. This lets tokens/history views
show offline workspaces or agents when they still have recorded usage.

## 7. Tests

Crate: `ax-usage` 테스트 (`crates/ax-usage/tests/{parse.rs,aggregator.rs}` +
소스 내 `#[cfg(test)]`).

- `project_dir_from_cwd` — golden cases for the `/`+`.` → `-` rule.
- parse 계열 — assistant 레코드 totals 파싱, user/attachment skip, malformed
  처리.
- aggregator 계열 — 다중 레코드 cumulative totals 및 `by_model` 그룹핑,
  가장 최근 assistant 레코드를 `current_context`로 채택.
- tail 계열 — offset 기반 증분 파싱, session 스위치 감지.

Fixtures under `crates/ax-usage/tests/fixtures/`.

No network, no tmpdir pollution beyond `t.TempDir()`.

## 8. Work breakdown / stages

1. **Stage 1 (this doc)** — design review. No code yet.
2. **Stage 2** — implement `ax-usage` parser + aggregator (no daemon wiring
   yet) with full unit tests under `crates/ax-usage/tests/fixtures/`.
3. **Stage 3** — wire scan into `ax-daemon` via `usage_trends` handler; add
   envelope `msg_get_workspace_usage` / `msg_usage_trends`; add MCP tool
   `get_workspace_usage`; integration smoke test via daemon test fixture.
   Watch/CLI surfacing deferred to a stage 3b or follow-up task.

Each stage ends with a mid-report to the root orchestrator. Commits are
gated on explicit user approval — working tree only.

## 9. Out of scope

- Historical cost trends / time-series persistence (only live cumulative).
- Enforcement / budget alerts.
- Context-window percentage display (requires per-model window map that
  shifts with upstream releases; deferred).
- Codex / non-claude transcript formats.
- Hook-based or statusline pipe collection (explicitly rejected by the
  delegation for install-cost reasons).

## 10. Open questions

- Should `get_workspace_usage` accept `workspace="all"`, or should we add
  `list_workspace_usage` as a separate bulk tool? Current plan: accept
  `all` to avoid proliferating tools. Can be split later without breaking
  the per-name case.
- Tick interval: 2s default. Worth exposing as `--usage-interval` flag on
  `ax daemon`? Deferred to stage 3 if it matters.
- Context-window table: hardcode in `ax-usage` or make config-driven?
  Deferred to watch/CLI surfacing stage.
