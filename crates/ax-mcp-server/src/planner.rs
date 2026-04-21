//! Read-only project surveyor + team planner. Powers the
//! `plan_initial_team` and `plan_team_reconfigure` MCP tools so an
//! orchestrator agent can ask "what's here?" and "what drifted?"
//! before choosing concrete changes to hand back through
//! `apply_team_reconfigure`.
//!
//! Everything in this module is deterministic: no subprocess, no
//! LLM call, no daemon roundtrip. We just walk the filesystem and
//! extract signals the caller can reason over.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use ax_config::Config;

const MANIFEST_HINTS: &[&str] = &[
    "package.json",
    "pnpm-workspace.yaml",
    "tsconfig.json",
    "go.mod",
    "go.sum",
    "Cargo.toml",
    "pyproject.toml",
    "requirements.txt",
    "Gemfile",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "main.tf",
    "terragrunt.hcl",
    "Dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
];

/// Names often associated with role-style partitioning. Used only
/// as a heuristic signal; final axis picking is the caller's job.
const ROLE_NAMES: &[&str] = &[
    "frontend",
    "backend",
    "api",
    "web",
    "ui",
    "client",
    "server",
    "cli",
    "infra",
    "infrastructure",
    "platform",
    "devops",
    "ops",
    "docs",
    "qa",
    "testing",
    "mobile",
];

/// Directory names we never treat as a workspace candidate.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".ax",
    ".github",
    ".vscode",
    ".idea",
    ".venv",
    "venv",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".terraform",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirSummary {
    pub name: String,
    /// Count of regular files immediately under this directory
    /// (non-recursive).
    pub file_count: u64,
    /// Manifest files detected at the top of this directory
    /// (package.json, go.mod, main.tf, ...).
    pub manifests: Vec<String>,
    /// Whether the directory has its own README at the top level.
    pub has_readme: bool,
    /// Whether the directory name matches a known role label.
    pub looks_role_like: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub name: String,
    pub dir: String,
    pub description: String,
    /// Whether the resolved directory exists on disk.
    pub exists: bool,
    /// Whether the resolved directory contains any non-hidden entries.
    pub non_empty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitialTeamPlan {
    pub project_dir: String,
    /// Heuristic axis suggestion: "role", "domain", or "hybrid".
    /// The caller may override based on its own reasoning.
    pub suggested_axis: String,
    /// Short bullet-point reasons that pushed the heuristic toward
    /// the suggested axis. Caller surfaces these back to the user.
    pub axis_signals: Vec<String>,
    pub toplevel_dirs: Vec<DirSummary>,
    pub readme_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconfigureTeamPlan {
    pub project_dir: String,
    /// Value of the `# axis:` comment at the top of the existing
    /// config, if present.
    pub current_axis: Option<String>,
    /// Rationale line if present (the `# rationale:` comment).
    pub current_rationale: Option<String>,
    pub current_workspaces: Vec<WorkspaceSummary>,
    pub toplevel_dirs: Vec<DirSummary>,
    /// Top-level directories visible in the repo that aren't owned
    /// by any configured workspace. Primary "add a workspace"
    /// hint for the caller.
    pub orphan_dirs: Vec<String>,
    /// Workspaces whose `dir` doesn't exist or is empty — candidates
    /// for `remove`.
    pub empty_workspaces: Vec<String>,
}

/// Survey `project_dir` and return the context an agent needs to
/// propose an initial team layout.
pub fn plan_initial_team(project_dir: &Path) -> std::io::Result<InitialTeamPlan> {
    let toplevel = scan_toplevel(project_dir)?;
    let (suggested_axis, axis_signals) = infer_axis(&toplevel);
    let readme_excerpt = read_excerpt(&project_dir.join("README.md"), 400);
    Ok(InitialTeamPlan {
        project_dir: project_dir.display().to_string(),
        suggested_axis,
        axis_signals,
        toplevel_dirs: toplevel,
        readme_excerpt,
    })
}

/// Survey `project_dir` and compare against the existing config
/// at `config_path` to surface drift the caller can act on.
pub fn plan_team_reconfigure(
    project_dir: &Path,
    config_path: &Path,
) -> std::io::Result<ReconfigureTeamPlan> {
    let cfg_body = std::fs::read_to_string(config_path)?;
    let (current_axis, current_rationale) = parse_axis_headers(&cfg_body);

    let cfg: Config = serde_yml::from_str(&cfg_body).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("parse config: {e}"),
        )
    })?;

    let toplevel = scan_toplevel(project_dir)?;

    let mut workspace_summaries = Vec::new();
    let mut covered_dirs = Vec::new();
    for (name, ws) in &cfg.workspaces {
        let resolved = resolve_workspace_dir(project_dir, &ws.dir);
        let exists = resolved.exists();
        let non_empty = exists && directory_non_empty(&resolved);
        if let Some(first) = first_path_component(&ws.dir) {
            covered_dirs.push(first);
        }
        workspace_summaries.push(WorkspaceSummary {
            name: name.clone(),
            dir: ws.dir.clone(),
            description: ws.description.clone(),
            exists,
            non_empty,
        });
    }

    let orphan_dirs = toplevel
        .iter()
        .filter(|d| !covered_dirs.iter().any(|c| c == &d.name))
        .map(|d| d.name.clone())
        .collect();

    let empty_workspaces = workspace_summaries
        .iter()
        .filter(|w| !w.exists || !w.non_empty)
        .map(|w| w.name.clone())
        .collect();

    Ok(ReconfigureTeamPlan {
        project_dir: project_dir.display().to_string(),
        current_axis,
        current_rationale,
        current_workspaces: workspace_summaries,
        toplevel_dirs: toplevel,
        orphan_dirs,
        empty_workspaces,
    })
}

// ---------- private helpers ----------

fn scan_toplevel(dir: &Path) -> std::io::Result<Vec<DirSummary>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if IGNORED_DIRS.iter().any(|i| *i == name) {
            continue;
        }
        let path = entry.path();
        let mut manifests = Vec::new();
        let mut file_count: u64 = 0;
        let mut has_readme = false;
        if let Ok(read) = std::fs::read_dir(&path) {
            for child in read.flatten() {
                let child_name = child.file_name().to_string_lossy().into_owned();
                if child.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    file_count += 1;
                    let lower = child_name.to_ascii_lowercase();
                    if lower == "readme.md" || lower == "readme" {
                        has_readme = true;
                    }
                    if MANIFEST_HINTS.iter().any(|m| *m == child_name) {
                        manifests.push(child_name);
                    }
                }
            }
        }
        let looks_role_like = ROLE_NAMES.iter().any(|r| r.eq_ignore_ascii_case(&name));
        out.push(DirSummary {
            name,
            file_count,
            manifests,
            has_readme,
            looks_role_like,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn infer_axis(dirs: &[DirSummary]) -> (String, Vec<String>) {
    let mut signals: Vec<String> = Vec::new();
    let role_hits = dirs.iter().filter(|d| d.looks_role_like).count();
    let non_role = dirs.iter().filter(|d| !d.looks_role_like).count();
    let with_readme = dirs.iter().filter(|d| d.has_readme).count();

    if role_hits >= 2 && role_hits >= non_role {
        signals.push(format!(
            "{role_hits} top-level directory name(s) match known role labels"
        ));
        return ("role".into(), signals);
    }
    if non_role >= 2 && with_readme >= 2 && role_hits == 0 {
        signals.push(format!(
            "{with_readme} top-level non-role directories carry their own README (bounded-context shape)"
        ));
        return ("domain".into(), signals);
    }
    if role_hits >= 1 && non_role >= 2 {
        signals.push(format!(
            "mix of {role_hits} role-shaped and {non_role} non-role directories suggests hybrid"
        ));
        return ("hybrid".into(), signals);
    }
    signals.push(
        "no strong axis signal from directory layout; default to domain with explicit rationale"
            .into(),
    );
    ("domain".into(), signals)
}

fn parse_axis_headers(yaml: &str) -> (Option<String>, Option<String>) {
    let mut axis = None;
    let mut rationale = None;
    for line in yaml.lines().take(15) {
        let Some(body) = line.strip_prefix('#') else {
            continue;
        };
        let trimmed = body.trim();
        let lower = trimmed.to_ascii_lowercase();
        if axis.is_none() {
            if let Some(rest) = lower.strip_prefix("axis:") {
                axis = rest
                    .split_whitespace()
                    .next()
                    .map(std::string::ToString::to_string);
                continue;
            }
        }
        if rationale.is_none() && lower.starts_with("rationale:") {
            let original_body = trimmed["rationale:".len()..].trim();
            if !original_body.is_empty() {
                rationale = Some(original_body.to_owned());
            }
        }
    }
    (axis, rationale)
}

fn resolve_workspace_dir(project_dir: &Path, ws_dir: &str) -> PathBuf {
    let raw = ws_dir.trim();
    if raw.is_empty() {
        return project_dir.to_path_buf();
    }
    let p = Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    project_dir.join(raw)
}

fn first_path_component(ws_dir: &str) -> Option<String> {
    let stripped = ws_dir.trim().trim_start_matches("./");
    let first = stripped.split('/').next()?;
    if first.is_empty() || first == "." {
        return None;
    }
    Some(first.to_owned())
}

fn directory_non_empty(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(read) => read
            .flatten()
            .any(|e| !e.file_name().to_string_lossy().starts_with('.')),
        Err(_) => false,
    }
}

fn read_excerpt(path: &Path, max_bytes: usize) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= max_bytes {
        return Some(trimmed.to_owned());
    }
    // Byte-boundary safe truncation.
    let mut cut = max_bytes;
    while !trimmed.is_char_boundary(cut) && cut > 0 {
        cut -= 1;
    }
    let mut s = trimmed[..cut].to_owned();
    s.push('…');
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mkdir(root: &Path, rel: &str) {
        std::fs::create_dir_all(root.join(rel)).unwrap();
    }
    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn scan_toplevel_filters_ignored_and_dotdirs() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        mkdir(p, "frontend");
        mkdir(p, "backend");
        mkdir(p, ".git");
        mkdir(p, "node_modules");
        mkdir(p, "target");
        write(p, "frontend/package.json", "{}");
        write(p, "backend/go.mod", "module x\n");

        let dirs = scan_toplevel(p).unwrap();
        let names: Vec<_> = dirs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["backend", "frontend"]);
        let fe = dirs.iter().find(|d| d.name == "frontend").unwrap();
        assert!(fe.manifests.contains(&"package.json".to_owned()));
        assert!(fe.looks_role_like);
    }

    #[test]
    fn infer_axis_picks_role_when_dirs_match_known_labels() {
        let dirs = vec![
            DirSummary {
                name: "frontend".into(),
                file_count: 1,
                manifests: vec!["package.json".into()],
                has_readme: false,
                looks_role_like: true,
            },
            DirSummary {
                name: "backend".into(),
                file_count: 1,
                manifests: vec!["go.mod".into()],
                has_readme: false,
                looks_role_like: true,
            },
        ];
        let (axis, signals) = infer_axis(&dirs);
        assert_eq!(axis, "role");
        assert!(!signals.is_empty());
    }

    #[test]
    fn infer_axis_picks_domain_when_bounded_context_shaped() {
        let dirs = vec![
            DirSummary {
                name: "users".into(),
                file_count: 2,
                manifests: vec![],
                has_readme: true,
                looks_role_like: false,
            },
            DirSummary {
                name: "orders".into(),
                file_count: 2,
                manifests: vec![],
                has_readme: true,
                looks_role_like: false,
            },
            DirSummary {
                name: "inventory".into(),
                file_count: 2,
                manifests: vec![],
                has_readme: true,
                looks_role_like: false,
            },
        ];
        let (axis, _) = infer_axis(&dirs);
        assert_eq!(axis, "domain");
    }

    #[test]
    fn infer_axis_picks_hybrid_on_mixed_layout() {
        let dirs = vec![
            DirSummary {
                name: "platform".into(),
                file_count: 1,
                manifests: vec![],
                has_readme: false,
                looks_role_like: true,
            },
            DirSummary {
                name: "users".into(),
                file_count: 1,
                manifests: vec![],
                has_readme: false,
                looks_role_like: false,
            },
            DirSummary {
                name: "billing".into(),
                file_count: 1,
                manifests: vec![],
                has_readme: false,
                looks_role_like: false,
            },
        ];
        let (axis, _) = infer_axis(&dirs);
        assert_eq!(axis, "hybrid");
    }

    #[test]
    fn plan_initial_team_surfaces_axis_and_readme_excerpt() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        mkdir(p, "frontend");
        mkdir(p, "backend");
        write(p, "frontend/package.json", "{}");
        write(p, "backend/go.mod", "module x\n");
        write(p, "README.md", "# demo\n\nA monorepo split by layer.\n");

        let plan = plan_initial_team(p).unwrap();
        assert_eq!(plan.suggested_axis, "role");
        assert!(plan.readme_excerpt.as_deref().unwrap().contains("monorepo"));
        assert_eq!(plan.toplevel_dirs.len(), 2);
    }

    #[test]
    fn parse_axis_headers_reads_both_comments() {
        let yaml = "# axis: domain\n# rationale: bounded contexts per service\nproject: shop\n";
        let (axis, rat) = parse_axis_headers(yaml);
        assert_eq!(axis.as_deref(), Some("domain"));
        assert_eq!(rat.as_deref(), Some("bounded contexts per service"));
    }

    #[test]
    fn plan_team_reconfigure_reports_orphan_and_empty_workspaces() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        mkdir(p, "frontend");
        mkdir(p, "backend");
        mkdir(p, "infra");
        // frontend + backend have content; backend counts as populated.
        write(p, "frontend/package.json", "{}");
        write(p, "backend/go.mod", "module x\n");
        write(p, "infra/main.tf", "provider \"aws\" {}\n");
        mkdir(p, ".ax");
        let cfg = "# axis: role\n# rationale: split by layer.\nproject: shop\n\
workspaces:\n  frontend:\n    dir: ./frontend\n    description: UI layer\n    runtime: codex\n  \
empty_legacy:\n    dir: ./gone\n    description: removed\n    runtime: codex\n";
        write(p, ".ax/config.yaml", cfg);

        let plan = plan_team_reconfigure(p, &p.join(".ax/config.yaml")).unwrap();
        assert_eq!(plan.current_axis.as_deref(), Some("role"));
        assert!(plan.orphan_dirs.contains(&"backend".to_owned()));
        assert!(plan.orphan_dirs.contains(&"infra".to_owned()));
        assert!(plan.empty_workspaces.contains(&"empty_legacy".to_owned()));
    }
}
