//! Probe that runs the real `ax-usage` pipeline against the operator's
//! local Claude/Codex transcripts. Gated on `AX_USAGE_PROBE=1` so it
//! never runs under `cargo test` by default.
//!
//! Invocation:
//!
//!   AX_USAGE_PROBE=1 cargo test -p ax-e2e --test usage_probe -- --nocapture
//!
//! Optional env knobs:
//! - `AX_USAGE_PROBE_CONFIG`: path to the `.ax/config.yaml` to use
//!   (default: walks up from CWD via `find_config_file`).
//! - `AX_USAGE_PROBE_SINCE_MINUTES`: look-back window (default: 3 days).
//! - `AX_USAGE_PROBE_BUCKET_MINUTES`: trend bucket size (default: 5).

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;

use ax_agent::{claude_project_path, discover_codex_home_candidates};
use ax_config::{find_config_file, Config};
use ax_usage::{query_history, HistoryQuery, HistoryResponse, WorkspaceBinding};

fn probe_enabled() -> bool {
    std::env::var("AX_USAGE_PROBE")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

fn env_duration_minutes(name: &str, default_minutes: u64) -> Duration {
    let minutes = std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default_minutes);
    Duration::from_secs(minutes * 60)
}

fn resolve_config_path() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("AX_USAGE_PROBE_CONFIG") {
        if !raw.is_empty() {
            return Some(PathBuf::from(raw));
        }
    }
    let cwd = std::env::current_dir().ok()?;
    find_config_file(&cwd)
}

#[test]
fn probe_local_usage_pipeline() {
    if !probe_enabled() {
        eprintln!("skip: set AX_USAGE_PROBE=1 to run this probe");
        return;
    }

    let config_path =
        resolve_config_path().expect("no .ax/config.yaml found; set AX_USAGE_PROBE_CONFIG");
    eprintln!("probe config: {}", config_path.display());

    let cfg = Config::load(&config_path).expect("load config");
    eprintln!("project: {}", cfg.project);
    eprintln!("workspaces: {}", cfg.workspaces.len());

    let project_dir = config_path
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_default();

    let bindings: Vec<WorkspaceBinding> = cfg
        .workspaces
        .iter()
        .map(|(name, ws)| {
            let dir = if std::path::Path::new(&ws.dir).is_absolute() {
                PathBuf::from(&ws.dir)
            } else {
                project_dir.join(&ws.dir)
            };
            let claude = claude_project_path(&dir).ok();
            let codex_homes =
                discover_codex_home_candidates(name, dir.to_str().unwrap_or_default())
                    .unwrap_or_default();
            eprintln!(
                "  binding {name}: dir={} claude={} codex_homes={}",
                dir.display(),
                claude
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "∅".to_owned()),
                codex_homes.len(),
            );
            for (idx, home) in codex_homes.iter().enumerate() {
                let kind = if idx == 0 { "canonical" } else { "legacy" };
                eprintln!(
                    "     {kind}: {} (exists={})",
                    home.display(),
                    home.exists()
                );
            }
            WorkspaceBinding {
                name: name.clone(),
                dir: dir.display().to_string(),
                claude_project_dir: claude,
                codex_homes,
            }
        })
        .collect();

    let now = Utc::now();
    let since_window = env_duration_minutes("AX_USAGE_PROBE_SINCE_MINUTES", 3 * 24 * 60);
    let bucket = env_duration_minutes("AX_USAGE_PROBE_BUCKET_MINUTES", 5);
    let query = HistoryQuery {
        since: now - chrono::Duration::from_std(since_window).unwrap(),
        until: now,
        bucket_size: bucket,
    }
    .normalized(now);

    let resp: HistoryResponse = query_history(&bindings, &query).expect("query_history");

    eprintln!(
        "\nquery window: {} → {}  (bucket {} min)",
        resp.since, resp.until, resp.bucket_minutes
    );
    eprintln!("workspaces: {}", resp.workspaces.len());
    for ws in &resp.workspaces {
        eprintln!("\n── {} ({}) ──", ws.workspace, ws.dir);
        eprintln!(
            "   available: {}  reason: {:?}",
            ws.available, ws.unavailable_reason
        );
        eprintln!(
            "   current: input={} output={} cache_read={} cache_create={} total={} turns={}",
            ws.current_snapshot.current_context.input,
            ws.current_snapshot.current_context.output,
            ws.current_snapshot.current_context.cache_read,
            ws.current_snapshot.current_context.cache_creation,
            ws.current_snapshot.current_total,
            ws.current_snapshot.turns,
        );
        eprintln!(
            "   cumulative: input={} output={} cache_read={} cache_create={} total={} turns={}",
            ws.current_snapshot.cumulative_totals.input,
            ws.current_snapshot.cumulative_totals.output,
            ws.current_snapshot.cumulative_totals.cache_read,
            ws.current_snapshot.cumulative_totals.cache_creation,
            ws.current_snapshot.cumulative_total,
            ws.current_snapshot.turns,
        );
        eprintln!("   buckets: {}", ws.recent_buckets.len());
        eprintln!("   agents: {}", ws.agents.len());
        for agent in &ws.agents {
            eprintln!(
                "     · {:<20} available={:<5} transcripts={} current_total={} turns={}",
                agent.agent,
                agent.available,
                agent.source_transcript_count,
                agent.current_snapshot.current_total,
                agent.current_snapshot.turns,
            );
        }
    }
}
