//! `usage_trends` handler. Resolves Claude project + `CODEX_HOME` paths
//! for each requested workspace via `ax_agent`, builds the
//! `WorkspaceBinding` list, and calls through to
//! `ax_usage::query_workspace_trends_for`.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;

use ax_proto::payloads::UsageTrendsPayload;
use ax_proto::responses::UsageTrendsResponse;
use ax_proto::Envelope;
use ax_usage::{query_workspace_trends_for, WorkspaceBinding};

use crate::handlers::{response_envelope, HandlerError};

pub(crate) fn handle_usage_trends(env: &Envelope) -> Result<Envelope, HandlerError> {
    let payload: UsageTrendsPayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("usage_trends", e))?;

    let since = Duration::from_secs((payload.since_minutes.max(0) as u64).saturating_mul(60));
    let bucket = Duration::from_secs((payload.bucket_minutes.max(0) as u64).saturating_mul(60));

    let mut bindings = Vec::with_capacity(payload.workspaces.len());
    for req in &payload.workspaces {
        bindings.push(build_binding(&req.workspace, &req.cwd));
    }

    let trends = query_workspace_trends_for(&bindings, Utc::now(), since, bucket)
        .map_err(|e| HandlerError::Logic(format!("query usage_trends: {e}")))?;
    response_envelope(&env.id, &UsageTrendsResponse { trends })
}

/// Resolve the optional `claude_project_dir` + every known
/// `CODEX_HOME` for a single `(workspace, cwd)` request. Missing
/// `HOME` or empty `cwd` leaves the corresponding fields empty — the
/// ax-usage scanner then falls through to its "no transcripts" branch
/// for that binding.
///
/// `codex_homes` includes the canonical path **and** any legacy
/// sibling directories so sessions rolled out before the key
/// normalisation still surface in the usage reply.
fn build_binding(workspace: &str, cwd: &str) -> WorkspaceBinding {
    let claude_project_dir: Option<PathBuf> = if cwd.is_empty() {
        None
    } else {
        ax_agent::claude_project_path(std::path::Path::new(cwd)).ok()
    };
    let codex_homes: Vec<PathBuf> = if cwd.is_empty() {
        Vec::new()
    } else {
        ax_agent::discover_codex_home_candidates(workspace, cwd).unwrap_or_default()
    };
    WorkspaceBinding {
        name: workspace.to_owned(),
        dir: cwd.to_owned(),
        claude_project_dir,
        codex_homes,
    }
}
