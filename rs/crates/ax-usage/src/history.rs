//! Claude transcript directory scan + per-transcript series.
//!
//! Initial slice: single-binding flow (enough to prove end-to-end Claude
//! → bucketed usage for one workspace). Multi-binding attribution (hint
//! matching, shared cwd, cross-workspace session ids) and the Codex
//! integration land in the next slice.
//!
//! Port tracks `internal/usage/history.go`; names match where practical.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};

use ax_proto::usage::{MCPProxyMetrics, Tokens};

use crate::aggregator::Aggregator;
use crate::parse::{parse_line, ParseError, ParsedRecord};

/// Default 3-hour look-back window if the caller didn't specify one.
pub const DEFAULT_HISTORY_WINDOW: Duration = Duration::from_secs(3 * 60 * 60);
/// Default 5-minute bucket size.
pub const DEFAULT_BUCKET_SIZE: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("read transcript {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse transcript line: {0}")]
    Parse(#[from] ParseError),
}

/// Parameters for a bounded history lookup.
#[derive(Debug, Clone)]
pub struct HistoryQuery {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub bucket_size: Duration,
}

impl HistoryQuery {
    /// Fills in the Go-compatible defaults for zero values: a 5-minute
    /// bucket, a 3-hour window ending at `now`.
    #[must_use]
    pub fn normalized(mut self, now: DateTime<Utc>) -> Self {
        if self.bucket_size.as_secs() == 0 {
            self.bucket_size = DEFAULT_BUCKET_SIZE;
        }
        if self.until.timestamp() == 0 {
            self.until = now;
        }
        if self.since.timestamp() == 0 || self.since >= self.until {
            let window = chrono::Duration::from_std(DEFAULT_HISTORY_WINDOW).unwrap();
            self.since = self.until - window;
        }
        self
    }
}

/// One fixed-width time bucket.
#[derive(Debug, Clone, Default)]
pub struct Bucket {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub tokens: Tokens,
    pub total: i64,
    pub mcp_proxy: MCPProxyMetrics,
    pub turns: i64,
}

/// Snapshot of the latest usage state for an agent or workspace.
#[derive(Debug, Clone, Default)]
pub struct CurrentSnapshot {
    pub last_activity: Option<DateTime<Utc>>,
    pub current_context: Tokens,
    pub current_total: i64,
    pub current_mcp_proxy: MCPProxyMetrics,
    pub current_model: String,
    pub cumulative_totals: Tokens,
    pub cumulative_total: i64,
    pub cumulative_mcp_proxy: MCPProxyMetrics,
    pub turns: i64,
}

/// Per-agent rollup inside a workspace.
#[derive(Debug, Clone, Default)]
pub struct AgentHistory {
    pub agent: String,
    pub available: bool,
    pub latest_session_id: String,
    pub latest_transcript: String,
    pub current_snapshot: CurrentSnapshot,
    pub recent_buckets: Vec<Bucket>,
    pub source_transcript_count: i64,
}

/// Workspace-level rollup returned to callers.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceHistory {
    pub workspace: String,
    pub dir: String,
    pub available: bool,
    pub unavailable_reason: String,
    pub current_snapshot: CurrentSnapshot,
    pub recent_buckets: Vec<Bucket>,
    pub agents: Vec<AgentHistory>,
}

/// One transcript file's scan result. Exposed at `pub(crate)` so the
/// codex scanner can build these too; callers outside the crate only
/// ever see the rolled-up [`WorkspaceHistory`].
#[derive(Debug, Default, Clone)]
pub(crate) struct TranscriptSeries {
    pub cwd: String,
    pub session_id: String,
    pub agent: String,
    pub workspace_hint: String,
    pub transcript: PathBuf,
    pub current: CurrentSnapshot,
    pub buckets: Vec<Bucket>,
}

/// Daemon's view of one ax workspace we want usage data for.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceBinding {
    pub name: String,
    pub dir: String,
    /// Resolved Claude project directory. Callers usually derive this
    /// via `ax_agent::claude_project_path`.
    pub claude_project_dir: Option<PathBuf>,
    /// Resolved `CODEX_HOME`. Callers usually derive this via
    /// `ax_agent::codex_home_path`.
    pub codex_home: Option<PathBuf>,
}

/// Multi-binding response returned by [`query_history`].
#[derive(Debug, Clone, Default)]
pub struct HistoryResponse {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub bucket_minutes: i64,
    pub workspaces: Vec<WorkspaceHistory>,
}

// ---------- public single-binding entry point ----------

/// Scan every `*.jsonl` transcript under `project_dir` and roll the
/// results into a single [`WorkspaceHistory`] attributed to
/// `workspace`. `dir` is the workspace's own directory; it's stored on
/// the returned value so callers can render the binding.
///
/// Missing `project_dir` yields `available = false` with the same
/// reason codes Go emits (`missing_workspace_dir`, `no_project_transcripts`,
/// `no_transcripts`). The full multi-binding / hint-matching flow lands
/// in a later slice.
pub fn scan_workspace_from_project_dir(
    workspace: &str,
    dir: &str,
    project_dir: &Path,
    query: &HistoryQuery,
) -> Result<WorkspaceHistory, HistoryError> {
    let mut out = WorkspaceHistory {
        workspace: workspace.to_owned(),
        dir: dir.to_owned(),
        ..WorkspaceHistory::default()
    };
    if dir.is_empty() {
        "missing_workspace_dir".clone_into(&mut out.unavailable_reason);
        return Ok(out);
    }
    if !project_dir.exists() {
        "no_project_transcripts".clone_into(&mut out.unavailable_reason);
        return Ok(out);
    }
    let paths = discover_transcripts(project_dir)?;
    if paths.is_empty() {
        "no_transcripts".clone_into(&mut out.unavailable_reason);
        return Ok(out);
    }

    let mut series = Vec::new();
    for path in paths {
        if let Some(s) = scan_transcript(&path, query)? {
            series.push(s);
        }
    }

    if series.is_empty() {
        "workspace_unattributed".clone_into(&mut out.unavailable_reason);
        return Ok(out);
    }

    let agents = build_agent_histories(&series, query.bucket_size);
    out.available = true;
    out.recent_buckets = aggregate_buckets(
        agents.iter().map(|a| a.recent_buckets.clone()).collect(),
        query.bucket_size,
    );
    out.current_snapshot =
        aggregate_snapshots(agents.iter().map(|a| a.current_snapshot.clone()).collect());
    out.agents = agents;
    Ok(out)
}

/// List `*.jsonl` files directly under `project_dir`. Entry order is
/// alphabetised to match Go's `sort.Strings`.
pub fn discover_transcripts(project_dir: &Path) -> Result<Vec<PathBuf>, HistoryError> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                walk(&path, out)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut paths = Vec::new();
    walk(project_dir, &mut paths).map_err(|e| HistoryError::Read {
        path: project_dir.display().to_string(),
        source: e,
    })?;
    paths.sort();
    Ok(paths)
}

/// Read one transcript file, aggregate usage, and bucket records inside
/// the query window. Returns `None` if the file was empty of signal —
/// no usage, session id, cwd, or workspace hint.
pub(crate) fn scan_transcript(
    path: &Path,
    query: &HistoryQuery,
) -> Result<Option<TranscriptSeries>, HistoryError> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).map_err(|e| HistoryError::Read {
        path: path.display().to_string(),
        source: e,
    })?;
    let reader = std::io::BufReader::new(file);

    let mut agg = Aggregator::new();
    let mut buckets: BTreeMap<DateTime<Utc>, Bucket> = BTreeMap::new();
    let mut series = TranscriptSeries {
        agent: agent_from_transcript_path(path),
        transcript: path.to_path_buf(),
        ..TranscriptSeries::default()
    };

    for line in reader.lines() {
        let line = line.map_err(|e| HistoryError::Read {
            path: path.display().to_string(),
            source: e,
        })?;
        // Malformed lines silently skipped to match Go.
        let Ok(rec) = parse_line(line.as_bytes()) else {
            continue;
        };
        merge_series_metadata(&mut series, &rec);
        let effect = agg.ingest(&rec);
        let Some(ts) = rec.timestamp else { continue };
        if ts < query.since || ts >= query.until {
            continue;
        }
        let tokens_delta_nonzero = effect.tokens_delta != Tokens::default();
        let mcp_delta_nonzero = effect.mcp_delta != MCPProxyMetrics::default();
        if effect.turn_delta == 0 && !tokens_delta_nonzero && !mcp_delta_nonzero {
            continue;
        }
        let start = truncate_to_bucket(ts, query.bucket_size);
        let b = buckets.entry(start).or_insert_with(|| Bucket {
            start,
            end: start
                + chrono::Duration::from_std(query.bucket_size).unwrap_or(chrono::Duration::zero()),
            ..Bucket::default()
        });
        if tokens_delta_nonzero {
            b.tokens = b.tokens + effect.tokens_delta;
            b.total += effect.tokens_delta.total();
        }
        if effect.turn_delta != 0 {
            b.turns += effect.turn_delta;
        }
        if mcp_delta_nonzero {
            b.mcp_proxy = add_mcp(b.mcp_proxy, effect.mcp_delta);
        }
    }

    if series.agent.is_empty() {
        "main".clone_into(&mut series.agent);
    }

    let snap = agg.snapshot("", &series.transcript.display().to_string());
    if !snap.available
        && series.workspace_hint.is_empty()
        && series.session_id.is_empty()
        && series.cwd.is_empty()
    {
        return Ok(None);
    }
    series.current = CurrentSnapshot {
        last_activity: snap.last_activity,
        current_context: snap.current_context,
        current_total: snap.current_context.total(),
        current_mcp_proxy: snap.current_mcp,
        current_model: snap.current_model,
        cumulative_totals: snap.cumulative_totals,
        cumulative_total: snap.cumulative_totals.total(),
        cumulative_mcp_proxy: snap.cumulative_mcp,
        turns: snap.turns,
    };
    series.buckets = buckets.into_values().collect();
    Ok(Some(series))
}

fn merge_series_metadata(series: &mut TranscriptSeries, rec: &ParsedRecord) {
    if series.session_id.is_empty() && !rec.session_id.is_empty() {
        series.session_id.clone_from(&rec.session_id);
    }
    if series.cwd.is_empty() && !rec.cwd.is_empty() {
        series.cwd = clean_path(&rec.cwd);
    }
    if !rec.agent_id.is_empty() {
        series.agent.clone_from(&rec.agent_id);
    }
    if series.workspace_hint.is_empty() && !rec.workspace_hint.is_empty() {
        series.workspace_hint.clone_from(&rec.workspace_hint);
    }
}

// ---------- agent + workspace rollups ----------

fn build_agent_histories(series: &[TranscriptSeries], bucket_size: Duration) -> Vec<AgentHistory> {
    #[derive(Default)]
    struct Accumulator {
        current: CurrentSnapshot,
        buckets: Vec<Bucket>,
        latest_session: String,
        latest_path: String,
        source_count: i64,
    }
    let mut acc: BTreeMap<String, Accumulator> = BTreeMap::new();
    for s in series {
        let entry = acc.entry(s.agent.clone()).or_default();
        entry.buckets =
            aggregate_buckets(vec![entry.buckets.clone(), s.buckets.clone()], bucket_size);
        entry.source_count += 1;
        if newer_snapshot(s.current.last_activity, entry.current.last_activity) {
            entry.current = s.current.clone();
            entry.latest_session.clone_from(&s.session_id);
            entry.latest_path = s.transcript.display().to_string();
        }
    }
    let mut agents: Vec<AgentHistory> = acc
        .into_iter()
        .map(|(name, a)| AgentHistory {
            agent: name,
            available: true,
            latest_session_id: a.latest_session,
            latest_transcript: a.latest_path,
            current_snapshot: a.current,
            recent_buckets: a.buckets,
            source_transcript_count: a.source_count,
        })
        .collect();
    agents.sort_by(|a, b| match (a.agent.as_str(), b.agent.as_str()) {
        ("main", "main") => Ordering::Equal,
        ("main", _) => Ordering::Less,
        (_, "main") => Ordering::Greater,
        (x, y) => x.cmp(y),
    });
    agents
}

fn aggregate_buckets(all: Vec<Vec<Bucket>>, bucket_size: Duration) -> Vec<Bucket> {
    let mut by_start: BTreeMap<DateTime<Utc>, Bucket> = BTreeMap::new();
    let bucket_duration =
        chrono::Duration::from_std(bucket_size).unwrap_or(chrono::Duration::zero());
    for list in all {
        for bucket in list {
            if let Some(entry) = by_start.get_mut(&bucket.start) {
                entry.tokens = entry.tokens + bucket.tokens;
                entry.total += bucket.total;
                entry.mcp_proxy = add_mcp(entry.mcp_proxy, bucket.mcp_proxy);
                entry.turns += bucket.turns;
            } else {
                let mut copy = bucket.clone();
                if copy.end.timestamp() == 0 {
                    copy.end = copy.start + bucket_duration;
                }
                by_start.insert(bucket.start, copy);
            }
        }
    }
    by_start.into_values().collect()
}

fn aggregate_snapshots(snaps: Vec<CurrentSnapshot>) -> CurrentSnapshot {
    let mut current = CurrentSnapshot::default();
    for snap in snaps {
        current.current_context = current.current_context + snap.current_context;
        current.current_total += snap.current_total;
        current.current_mcp_proxy = add_mcp(current.current_mcp_proxy, snap.current_mcp_proxy);
        current.cumulative_totals = current.cumulative_totals + snap.cumulative_totals;
        current.cumulative_total += snap.cumulative_total;
        current.cumulative_mcp_proxy =
            add_mcp(current.cumulative_mcp_proxy, snap.cumulative_mcp_proxy);
        current.turns += snap.turns;
        if newer_snapshot(snap.last_activity, current.last_activity) {
            current.last_activity = snap.last_activity;
            current.current_model.clone_from(&snap.current_model);
        }
    }
    current
}

// ---------- helpers ----------

fn agent_from_transcript_path(path: &Path) -> String {
    let Some(base) = path.file_name().and_then(|s| s.to_str()) else {
        return "main".to_owned();
    };
    if let Some(stripped) = base
        .strip_prefix("agent-")
        .and_then(|s| s.strip_suffix(".jsonl"))
    {
        return stripped.to_owned();
    }
    "main".to_owned()
}

fn truncate_to_bucket(ts: DateTime<Utc>, bucket: Duration) -> DateTime<Utc> {
    let secs = bucket.as_secs() as i64;
    if secs == 0 {
        return ts;
    }
    let epoch = ts.timestamp();
    let truncated = epoch - epoch.rem_euclid(secs);
    DateTime::<Utc>::from_timestamp(truncated, 0).unwrap_or(ts)
}

fn clean_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    PathBuf::from(trimmed).to_string_lossy().into_owned()
}

fn newer_snapshot(candidate: Option<DateTime<Utc>>, current: Option<DateTime<Utc>>) -> bool {
    match (candidate, current) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(c), Some(cur)) => c > cur,
    }
}

fn add_mcp(a: MCPProxyMetrics, b: MCPProxyMetrics) -> MCPProxyMetrics {
    MCPProxyMetrics {
        total: a.total + b.total,
        prompt_tokens: a.prompt_tokens + b.prompt_tokens,
        prompt_signals: a.prompt_signals + b.prompt_signals,
        tool_use_tokens: a.tool_use_tokens + b.tool_use_tokens,
        tool_use_turns: a.tool_use_turns + b.tool_use_turns,
    }
}

// ---------- multi-binding entry point ----------

/// Scan every binding's Claude transcripts plus its Codex sessions and
/// return one [`WorkspaceHistory`] per binding. Mirrors Go's
/// `QueryHistory`.
///
/// Callers populate `binding.claude_project_dir` and/or
/// `binding.codex_home` before calling; missing paths simply mean "no
/// data from that runtime for this workspace".
pub fn query_history(
    bindings: &[WorkspaceBinding],
    query: &HistoryQuery,
) -> Result<HistoryResponse, HistoryError> {
    let bucket_minutes = (query.bucket_size.as_secs() / 60) as i64;
    let mut resp = HistoryResponse {
        since: query.since,
        until: query.until,
        bucket_minutes,
        workspaces: Vec::with_capacity(bindings.len()),
    };
    if bindings.is_empty() {
        return Ok(resp);
    }

    // Claude scan: group by dir so shared-cwd workspaces don't re-read
    // the same transcripts.
    let mut seen_dirs: std::collections::BTreeMap<String, DirScanState> =
        std::collections::BTreeMap::new();
    for binding in bindings {
        let key = binding.dir.clone();
        if seen_dirs.contains_key(&key) || binding.claude_project_dir.is_none() {
            continue;
        }
        let project_dir = binding.claude_project_dir.as_ref().unwrap();
        let mut state = DirScanState::default();
        if !project_dir.exists() {
            seen_dirs.insert(key, state);
            continue;
        }
        state.project_exists = true;
        let paths = discover_transcripts(project_dir)?;
        if !paths.is_empty() {
            state.transcripts_found = true;
            for path in paths {
                if let Some(series) = scan_transcript(&path, query)? {
                    state.series.push(series);
                }
            }
        }
        seen_dirs.insert(key, state);
    }

    // Codex scan per-binding (one CODEX_HOME per workspace).
    let mut codex_by_binding: std::collections::BTreeMap<String, Vec<TranscriptSeries>> =
        std::collections::BTreeMap::new();
    let mut codex_flags: std::collections::BTreeMap<String, (bool, bool)> =
        std::collections::BTreeMap::new();
    for binding in bindings {
        if let Some(home) = &binding.codex_home {
            let res = crate::codex::scan_codex_for_binding(&binding.name, home, query)?;
            codex_flags.insert(binding.name.clone(), (res.home_exists, res.sessions_found));
            if !res.series.is_empty() {
                codex_by_binding.insert(binding.name.clone(), res.series);
            }
        }
    }

    // Collect all Claude series across dirs for attribution.
    let mut all_claude: Vec<&TranscriptSeries> = Vec::new();
    for state in seen_dirs.values() {
        all_claude.extend(state.series.iter());
    }
    let assignments = assign_series(bindings, &all_claude);

    // Compose per-binding WorkspaceHistory.
    for binding in bindings {
        let mut ws = WorkspaceHistory {
            workspace: binding.name.clone(),
            dir: binding.dir.clone(),
            ..WorkspaceHistory::default()
        };
        let claude_series = assignments.get(&binding.name).cloned().unwrap_or_default();
        let mut series: Vec<TranscriptSeries> = claude_series.into_iter().cloned().collect();
        if let Some(codex_series) = codex_by_binding.remove(&binding.name) {
            series.extend(codex_series);
        }

        if series.is_empty() {
            let state = seen_dirs.get(&binding.dir).cloned().unwrap_or_default();
            let (codex_home_exists, codex_sessions_found) = codex_flags
                .get(&binding.name)
                .copied()
                .unwrap_or((false, false));
            ws.unavailable_reason = unavailable_reason_for(
                &binding.dir,
                state.project_exists,
                state.transcripts_found,
                codex_home_exists,
                codex_sessions_found,
            );
            resp.workspaces.push(ws);
            continue;
        }

        let agents = build_agent_histories(&series, query.bucket_size);
        ws.available = true;
        ws.recent_buckets = aggregate_buckets(
            agents.iter().map(|a| a.recent_buckets.clone()).collect(),
            query.bucket_size,
        );
        ws.current_snapshot =
            aggregate_snapshots(agents.iter().map(|a| a.current_snapshot.clone()).collect());
        ws.agents = agents;
        resp.workspaces.push(ws);
    }
    resp.workspaces
        .sort_by(|a, b| a.workspace.cmp(&b.workspace));
    Ok(resp)
}

/// Rebuild `WorkspaceHistory` into the ax-proto `WorkspaceTrend` shape the
/// daemon exposes over MCP. Keeps the library self-contained so callers
/// that just want usage numbers don't need ax-proto.
#[must_use]
pub fn query_workspace_trends(resp: &HistoryResponse) -> Vec<ax_proto::usage::WorkspaceTrend> {
    use ax_proto::usage::WorkspaceTrend;

    let bucket_duration = chrono::Duration::minutes(resp.bucket_minutes);
    resp.workspaces
        .iter()
        .map(|ws| {
            let mut trend = WorkspaceTrend {
                workspace: ws.workspace.clone(),
                cwd: ws.dir.clone(),
                available: ws.available,
                unavailable_reason: ws.unavailable_reason.clone(),
                window_start: resp.since,
                window_end: resp.until,
                bucket_minutes: resp.bucket_minutes,
                ..WorkspaceTrend::default()
            };
            if !ws.available {
                trend.error.clone_from(&ws.unavailable_reason);
                return trend;
            }
            trend.buckets = make_usage_buckets(&ws.recent_buckets, bucket_duration);
            trend.total = sum_bucket_totals(&trend.buckets);
            trend.mcp_proxy = sum_bucket_mcp(&trend.buckets);
            trend.last_activity = ws.current_snapshot.last_activity;
            trend.latest_tokens = ws.current_snapshot.current_context;
            trend.latest_mcp_proxy = ws.current_snapshot.current_mcp_proxy;
            trend
                .latest_model
                .clone_from(&ws.current_snapshot.current_model);
            trend.agents = make_agent_trends(&ws.agents, bucket_duration);
            trend
        })
        .collect()
}

/// Convenience wrapper matching Go's `usage.QueryWorkspaceTrends`
/// signature: builds the `HistoryQuery` window from `(now, since,
/// bucket)`, runs [`query_history`], and reshapes the result. Zero
/// `since` / `bucket` pick up the Go-compatible defaults.
pub fn query_workspace_trends_for(
    bindings: &[WorkspaceBinding],
    now: DateTime<Utc>,
    since: Duration,
    bucket: Duration,
) -> Result<Vec<ax_proto::usage::WorkspaceTrend>, HistoryError> {
    let since = if since.as_secs() == 0 {
        DEFAULT_HISTORY_WINDOW
    } else {
        since
    };
    let bucket = if bucket.as_secs() == 0 {
        DEFAULT_BUCKET_SIZE
    } else {
        bucket
    };
    let window = chrono::Duration::from_std(since).unwrap_or(chrono::Duration::zero());
    let query = HistoryQuery {
        since: now - window,
        until: now,
        bucket_size: bucket,
    };
    let resp = query_history(bindings, &query)?;
    Ok(query_workspace_trends(&resp))
}

fn make_usage_buckets(
    buckets: &[Bucket],
    bucket_duration: chrono::Duration,
) -> Vec<ax_proto::usage::UsageBucket> {
    buckets
        .iter()
        .map(|b| ax_proto::usage::UsageBucket {
            start: b.start,
            end: if b.end.timestamp() == 0 {
                b.start + bucket_duration
            } else {
                b.end
            },
            totals: b.tokens,
            mcp_proxy: b.mcp_proxy,
            turns: b.turns,
        })
        .collect()
}

fn make_agent_trends(
    agents: &[AgentHistory],
    bucket_duration: chrono::Duration,
) -> Vec<ax_proto::usage::AgentTrend> {
    agents
        .iter()
        .map(|a| {
            let buckets = make_usage_buckets(&a.recent_buckets, bucket_duration);
            let total = sum_bucket_totals(&buckets);
            let mcp_proxy = sum_bucket_mcp(&buckets);
            ax_proto::usage::AgentTrend {
                agent: a.agent.clone(),
                available: a.available,
                latest_session_id: a.latest_session_id.clone(),
                latest_transcript_path: a.latest_transcript.clone(),
                buckets,
                total,
                mcp_proxy,
                last_activity: a.current_snapshot.last_activity,
                latest_tokens: a.current_snapshot.current_context,
                latest_mcp_proxy: a.current_snapshot.current_mcp_proxy,
                latest_model: a.current_snapshot.current_model.clone(),
            }
        })
        .collect()
}

fn sum_bucket_totals(buckets: &[ax_proto::usage::UsageBucket]) -> Tokens {
    let mut total = Tokens::default();
    for b in buckets {
        total = total + b.totals;
    }
    total
}

fn sum_bucket_mcp(buckets: &[ax_proto::usage::UsageBucket]) -> MCPProxyMetrics {
    let mut total = MCPProxyMetrics::default();
    for b in buckets {
        total = add_mcp(total, b.mcp_proxy);
    }
    total
}

#[derive(Debug, Default, Clone)]
struct DirScanState {
    project_exists: bool,
    transcripts_found: bool,
    series: Vec<TranscriptSeries>,
}

#[allow(clippy::fn_params_excessive_bools)]
fn unavailable_reason_for(
    dir: &str,
    project_exists: bool,
    transcripts_found: bool,
    codex_home_exists: bool,
    codex_sessions_found: bool,
) -> String {
    if dir.is_empty() {
        return "missing_workspace_dir".to_owned();
    }
    if !project_exists && !codex_home_exists {
        return "no_project_transcripts".to_owned();
    }
    if !transcripts_found && !codex_sessions_found {
        return "no_transcripts".to_owned();
    }
    "workspace_unattributed".to_owned()
}

/// Attribute transcript series to bindings via the same three-step
/// heuristic Go uses: workspace hint first, then shared session id, and
/// finally a unique-cwd fallback.
fn assign_series<'a>(
    bindings: &[WorkspaceBinding],
    series: &'a [&'a TranscriptSeries],
) -> std::collections::BTreeMap<String, Vec<&'a TranscriptSeries>> {
    let mut assignments: std::collections::BTreeMap<String, Vec<&TranscriptSeries>> =
        std::collections::BTreeMap::new();
    let mut binding_by_name: std::collections::BTreeMap<String, &WorkspaceBinding> =
        std::collections::BTreeMap::new();
    let mut unique_by_dir: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    for binding in bindings {
        binding_by_name.insert(binding.name.clone(), binding);
        let dir = clean_path(&binding.dir);
        if dir.is_empty() {
            continue;
        }
        match unique_by_dir.get(&dir) {
            Some(existing) if existing != &binding.name => {
                // Mark as ambiguous.
                unique_by_dir.insert(dir, String::new());
            }
            Some(_) => {}
            None => {
                unique_by_dir.insert(dir, binding.name.clone());
            }
        }
    }

    // Pass 1: direct workspace-hint match.
    let mut session_workspace: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for s in series {
        if s.workspace_hint.is_empty() {
            continue;
        }
        let Some(binding) = binding_by_name.get(&s.workspace_hint) else {
            continue;
        };
        if !binding.dir.is_empty()
            && !s.cwd.is_empty()
            && clean_path(&binding.dir) != clean_path(&s.cwd)
        {
            continue;
        }
        assignments.entry(binding.name.clone()).or_default().push(s);
        if !s.session_id.is_empty() {
            session_workspace.insert(s.session_id.clone(), binding.name.clone());
        }
    }

    // Pass 2: hint-less series → session id → unique cwd.
    for s in series {
        if !s.workspace_hint.is_empty() {
            continue;
        }
        let mut workspace = String::new();
        if !s.session_id.is_empty() {
            if let Some(name) = session_workspace.get(&s.session_id) {
                workspace.clone_from(name);
            }
        }
        if workspace.is_empty() && !s.cwd.is_empty() {
            let dir = clean_path(&s.cwd);
            if let Some(name) = unique_by_dir.get(&dir) {
                if !name.is_empty() {
                    workspace.clone_from(name);
                }
            }
        }
        if workspace.is_empty() {
            continue;
        }
        assignments.entry(workspace).or_default().push(s);
    }

    assignments
}
