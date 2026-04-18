//! `ax-rs init` — port of `cmd/init_cmd.go`. Writes a minimal
//! `.ax/config.yaml`, registers the current directory as a child
//! of any ancestor/global config, refreshes the orchestrator tree,
//! adds `.mcp.json` to `.gitignore`, and optionally launches a
//! setup agent (Claude or Codex) to flesh out the workspace list.
//!
//! The Go version rendered a Bubbletea spinner while streaming the
//! Claude stream-json output. This port keeps the stream parsing so
//! progress text still shows up, but drops the animated spinner — the
//! intent is to let `ax init` stay usable from a terminal without
//! dragging in a TUI library. The full animated UX can come back
//! when the watch TUI ports to ratatui.
//!
//! Heavy UI is out of scope; this prioritises the scaffold-and-refresh
//! semantics so newly-registered sub-projects show up in the parent
//! orchestrator tree.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use ax_config::{default_config_path, legacy_config_path, Child, Config};
use ax_workspace::{ensure_orchestrator_tree, orchestrator_name, RealTmux};

use crate::daemon_client::DaemonClient;

/// Team-partitioning axis the setup agent should use when writing
/// workspaces. `Auto` is the default: the agent inspects the
/// project and picks role- or domain-centric based on observed
/// boundaries plus a Conway's Law prompt. `Role` / `Domain` force
/// the choice when the user already knows what they want.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    Auto,
    Role,
    Domain,
}

impl Axis {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "role" | "role-first" => Some(Self::Role),
            "domain" | "domain-first" => Some(Self::Domain),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent opt-in flags
pub(crate) struct InitOptions {
    pub global: bool,
    pub no_setup: bool,
    /// When `true`, skip walking the ancestor/global orchestrator
    /// tree after scaffolding. Useful for throwaway `ax init` runs
    /// where the user doesn't want every ancestor orchestrator
    /// session waking up.
    pub no_refresh: bool,
    pub runtime: String,
    pub socket_path: PathBuf,
    pub daemon_running: bool,
    pub axis: Axis,
}

pub(crate) fn run(opts: &InitOptions) -> Result<String, InitError> {
    let mut out = String::new();
    let dir = if opts.global {
        home_dir().ok_or(InitError::HomeDir)?
    } else {
        std::env::current_dir().map_err(InitError::Cwd)?
    };
    let path = default_config_path(&dir);
    let project_name = if opts.global {
        "global".to_owned()
    } else {
        dir.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };

    let already_exists = path.exists();
    if already_exists {
        let _ = writeln!(out, "{} already exists — skipping creation", path.display());
    } else if !opts.global {
        if let Some(conflict) = config_path_conflict(&dir) {
            return Err(InitError::LegacyConfigConflict(conflict));
        }
    }

    if !already_exists {
        let cfg = Config::default_for_runtime(&project_name, &opts.runtime);
        cfg.save(&path).map_err(InitError::SaveConfig)?;
        let _ = writeln!(out, "Created {}", path.display());
    }

    if !opts.global {
        if let Some(parent_path) = register_as_child(&dir, &project_name, &mut out)? {
            let _ = writeln!(out, "Registered as child of {}", parent_path.display());
        }
        if opts.no_refresh {
            out.push_str("note: orchestrator tree refresh skipped (--no-refresh)\n");
        } else if let Err(e) = refresh_orchestrator_tree(opts, &project_name) {
            let _ = writeln!(out, "note: orchestrator tree refresh skipped: {e}");
        }
    }

    if !opts.global && ensure_gitignore(&dir, ".mcp.json").map_err(InitError::Io)? {
        out.push_str("Added .mcp.json to .gitignore\n");
    }

    if already_exists || opts.no_setup {
        if !already_exists {
            out.push_str("Edit it to define your workspaces, then run: ax up\n");
        }
        return Ok(out);
    }

    let _ = writeln!(out, "\nLaunching setup agent ({})...", opts.runtime);
    out.push_str("The agent will analyze your project and help define workspaces.\n\n");
    // Print the pre-agent header up-front so the user sees status while the
    // subprocess streams its output.
    print!("{out}");
    std::io::Write::flush(&mut std::io::stdout()).ok();

    run_setup_agent(&dir, &path, &opts.runtime, opts.axis)?;
    Ok(String::new())
}

fn run_setup_agent(project_dir: &Path, config_path: &Path, runtime: &str, axis: Axis) -> Result<(), InitError> {
    let system_prompt = build_setup_system_prompt(config_path, runtime, axis);
    let user_prompt = "프로젝트 구조를 파악해서 워크스페이스 구성을 결정하고 config.yaml에 작성해주세요. 작성 완료 후 어떤 워크스페이스를 만들었는지 요약해주세요.";

    if runtime == "codex" {
        let Ok(bin) = which("codex") else {
            println!("codex CLI not found — skipping setup.");
            println!("Edit {} manually and run: ax up", config_path.display());
            return Ok(());
        };
        let prompt = format!("{system_prompt}\n\n## 사용자 요청\n{user_prompt}");
        let status = ProcessCommand::new(bin)
            .args([
                "exec",
                "--dangerously-bypass-approvals-and-sandbox",
                "--skip-git-repo-check",
                "-C",
            ])
            .arg(project_dir)
            .arg(prompt)
            .current_dir(project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(InitError::SpawnAgent)?;
        if !status.success() {
            return Err(InitError::AgentExit {
                runtime: "codex".into(),
                code: status.code(),
            });
        }
        return Ok(());
    }

    let Ok(bin) = which("claude") else {
        println!("claude CLI not found — skipping setup.");
        println!("Edit {} manually and run: ax up", config_path.display());
        return Ok(());
    };
    let mut child = ProcessCommand::new(bin)
        .args([
            "-p",
            "--dangerously-skip-permissions",
            "--output-format",
            "stream-json",
            "--verbose",
            "--append-system-prompt",
            &system_prompt,
            user_prompt,
        ])
        .current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(InitError::SpawnAgent)?;
    let final_text = if let Some(stdout) = child.stdout.take() {
        stream_claude_output(stdout)
    } else {
        String::new()
    };
    let status = child.wait().map_err(InitError::SpawnAgent)?;
    if !final_text.is_empty() {
        println!("{final_text}");
    }
    if !status.success() {
        return Err(InitError::AgentExit {
            runtime: "claude".into(),
            code: status.code(),
        });
    }
    Ok(())
}

fn stream_claude_output(r: impl std::io::Read) -> String {
    let reader = BufReader::new(r);
    let mut final_text = String::new();
    for line in reader.lines().map_while(Result::ok) {
        if line.is_empty() {
            continue;
        }
        let Ok(evt) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if evt.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let Some(content) = evt
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for item in content {
            match item.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            text.clone_into(&mut final_text);
                            let first = text.trim().lines().next().unwrap_or("").trim().to_owned();
                            let truncated = if first.chars().count() > 60 {
                                let mut s: String = first.chars().take(57).collect();
                                s.push_str("...");
                                s
                            } else {
                                first
                            };
                            eprintln!("[setup] {truncated}");
                        }
                    }
                }
                Some("tool_use") => {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    eprintln!("[setup] {}", describe_tool_use(name, item));
                }
                _ => {}
            }
        }
    }
    final_text
}

fn describe_tool_use(name: &str, block: &serde_json::Value) -> String {
    let input = block.get("input");
    let path_of = |field: &str| -> Option<String> {
        input
            .and_then(|i| i.get(field))
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
    };
    match name {
        "Read" => path_of("file_path").map_or_else(
            || format!("Using {name}"),
            |p| format!("Reading {}", short_path(&p)),
        ),
        "Write" => path_of("file_path").map_or_else(
            || format!("Using {name}"),
            |p| format!("Writing {}", short_path(&p)),
        ),
        "Edit" => path_of("file_path").map_or_else(
            || format!("Using {name}"),
            |p| format!("Editing {}", short_path(&p)),
        ),
        "Glob" => {
            path_of("pattern").map_or_else(|| format!("Using {name}"), |p| format!("Searching {p}"))
        }
        "Grep" => {
            path_of("pattern").map_or_else(|| format!("Using {name}"), |p| format!("Grepping {p}"))
        }
        "Bash" => path_of("description")
            .filter(|d| !d.is_empty())
            .map_or_else(|| format!("Using {name}"), |d| format!("Running: {d}")),
        _ => format!("Using {name}"),
    }
}

fn short_path(p: &str) -> String {
    let mut out = p.to_owned();
    if let Some(home) = home_dir() {
        let home_str = home.display().to_string();
        if out.starts_with(&home_str) {
            out = format!("~{}", &out[home_str.len()..]);
        }
    }
    let parts: Vec<&str> = out.split('/').collect();
    if parts.len() > 3 {
        format!(".../{}", parts[parts.len() - 3..].join("/"))
    } else {
        out
    }
}

fn build_setup_system_prompt(config_path: &Path, runtime: &str, axis: Axis) -> String {
    let cp = config_path.display();
    let axis_directive = match axis {
        Axis::Auto => AXIS_AUTO_DIRECTIVE,
        Axis::Role => AXIS_ROLE_DIRECTIVE,
        Axis::Domain => AXIS_DOMAIN_DIRECTIVE,
    };
    format!(
        "당신은 ax 프로젝트 셋업 에이전트입니다. 사용자가 요청하면 현재 디렉토리의 프로젝트를 분석해서 멀티 에이전트 워크스페이스 구성을 제안하고 {cp} 파일을 편집하세요.\n\n\
## Conway's Law — 축 선택이 가장 중요합니다\n\
지금 당신이 팀을 어떻게 쪼개느냐가 **앞으로 3~6개월 동안 이 코드베이스의 경계**가 됩니다. 한 번 정해진 축은 디렉토리 구조·모듈 인터페이스·커밋 경계를 따라가며 굳어지고, 재분할은 큰 비용이 들어요. 그래서 관찰 기반으로 신중히 고르되, 결정했으면 주저하지 마세요.\n\n\
### 축 기준\n\
- **도메인 중심** (예: `users`, `billing`, `inventory`) — 비즈니스 경계가 안정적이고, 각 도메인이 풀스택으로 독립 배포될 수 있고, 교차 기능 변경이 드물 때. 대부분 확장 지향 프로젝트에 적합.\n\
- **역할 중심** (예: `frontend`, `backend`, `infra`, `docs`) — 도메인 경계가 아직 미성숙하거나, 단일 코드베이스에서 역할별 전문성(보안·접근성·인프라)이 품질을 가르는 경우. 프로토타입·초기 단계에 자연스러움.\n\
- **하이브리드** — 도메인 팀 여러 개 + 얇은 횡단 팀 1~2개(platform/docs)로 혼합. 큰 조직에서 현실적.\n\n\
{axis_directive}\n\n\
## 절차\n\
1. **관찰**: 프로젝트 구조를 파악하세요 (Glob으로 디렉토리 구조, README/package.json/go.mod/pyproject.toml 등 주요 파일 확인). 기존에 이미 어떤 경계가 보이는지 — `app/` + `api/` 식 역할 분리인지, `users/` + `billing/` 식 도메인 분리인지, 아직 미분화인지 — 를 먼저 읽어내세요.\n\
2. **축 결정**: 위 기준과 관찰 결과를 바탕으로 축을 정하세요. 위 '축 지시' 섹션을 따릅니다.\n\
3. **근거 기록**: {cp} 파일 최상단에 다음 주석을 넣으세요.\n   \
```\n   \
# axis: role | domain | hybrid\n   \
# rationale: <왜 이 축을 골랐는지 1~2문장>\n   \
```\n\
4. **사용자에게 확인을 묻지 말고** 바로 {cp}의 `workspaces` 섹션을 채우세요.\n\
5. 편집 후 최종 구성과 축 선택 근거를 요약해서 보여주고, 사용자가 조정을 요청하면 반영하세요.\n\n\
## config.yaml 형식\n\
```yaml\n\
# axis: <선택한 축>\n\
# rationale: <1~2문장 근거>\n\
project: <프로젝트 이름>\n\
workspaces:\n  \
<name>:\n    \
dir: <프로젝트 루트 기준 상대 경로>\n    \
description: <해당 에이전트의 역할 한 문장; 왜 이 경계를 그었는지 포함>\n    \
runtime: {runtime}\n    \
instructions: |\n      \
<해당 워크스페이스 에이전트가 받을 지침 — 무엇을 해야 하는지, 어떤 파일을 건드려야 하는지, 어떤 경계를 넘지 말아야 하는지>\n\
```\n\n\
## 주의사항\n\
- 워크스페이스 이름은 선택한 축을 그대로 반영하세요 (도메인이면 도메인명, 역할이면 역할명). 축을 흐리는 중립적 이름(`worker1`, `module2`)은 피하세요.\n\
- description은 한 문장으로 **왜** 이 경계가 존재하는지 명확히 설명. 단순히 \"이 디렉토리를 담당\"이 아니라 \"결제 플로우 전반 담당\" / \"프론트엔드 UI와 상태관리 담당\" 같은 식으로.\n\
- 이 초기화에서는 기본 런타임을 {runtime}로 사용하세요.\n\
- instructions는 구체적으로 작성 — 그 에이전트가 어떤 디렉토리에서 작업하고, 어떤 파일·디렉토리를 건드리면 안 되는지까지.\n\
- 기존 {cp} 파일은 최소 stub만 있는 상태입니다. workspaces 섹션과 최상단 축 주석을 채워주세요."
    )
}

const AXIS_AUTO_DIRECTIVE: &str = "\
### 축 지시: `auto`\n\
관찰 결과를 바탕으로 에이전트(당신)가 직접 축을 결정하세요. 애매할 때 기본 선호는 **도메인 > 하이브리드 > 역할** 순서. 다만 프로젝트가 단일 언어·단일 배포이고 디렉토리가 이미 역할로 쪼개져 있다면(`frontend/` + `backend/`, `src/` + `infra/` 등) 역할 축이 자연스러울 수 있습니다. 선택한 축과 근거를 명시적으로 설명하세요.";

const AXIS_ROLE_DIRECTIVE: &str = "\
### 축 지시: `--axis role`\n\
사용자가 **역할 중심**을 지정했습니다. 관찰 결과와 무관하게 역할 축으로 구성하세요 (frontend/backend/infra/docs/qa 등 필요한 것만). 다만 rationale에는 관찰 결과와 왜 역할 축이 이 프로젝트에 맞는지(또는 사용자가 왜 이 축을 선택했을지)를 1~2문장으로 적어주세요.";

const AXIS_DOMAIN_DIRECTIVE: &str = "\
### 축 지시: `--axis domain`\n\
사용자가 **도메인 중심**을 지정했습니다. 관찰 결과에서 비즈니스 도메인을 추출해 도메인 이름(users, billing, inventory, ...)으로 팀을 구성하세요. 관찰에서 도메인이 뚜렷하지 않으면 가장 그럴듯한 도메인 분할을 제안하고 rationale에 \"현재 도메인 경계가 덜 명확하니 이후 재검토 권장\"을 명시하세요.";

fn register_as_child(
    child_dir: &Path,
    name: &str,
    log: &mut String,
) -> Result<Option<PathBuf>, InitError> {
    register_as_child_with_home(child_dir, name, home_dir(), log)
}

/// Extracted form that takes an explicit `home` override. Tests
/// drive this directly instead of racing against `env::set_var`.
fn register_as_child_with_home(
    child_dir: &Path,
    name: &str,
    home: Option<PathBuf>,
    log: &mut String,
) -> Result<Option<PathBuf>, InitError> {
    let mut top: Option<(PathBuf, PathBuf)> = None; // (config_path, parent_dir)

    let mut cur = child_dir.parent().map(Path::to_path_buf);
    while let Some(ref d) = cur {
        if let Some(path) = find_config_in_dir(d) {
            top = Some((path, d.clone()));
        }
        let next = d.parent().map(Path::to_path_buf);
        if next.as_deref() == cur.as_deref() {
            break;
        }
        cur = next;
    }
    if let Some(home) = home {
        if let Some(path) = find_config_in_dir(&home) {
            top = Some((path, home));
        }
    }

    let Some((cfg_path, parent_dir)) = top else {
        return Ok(None);
    };
    let added = add_child_to_config(&cfg_path, &parent_dir, child_dir, name, log)?;
    Ok(if added { Some(cfg_path) } else { None })
}

fn find_config_in_dir(dir: &Path) -> Option<PathBuf> {
    let preferred = default_config_path(dir);
    if preferred.exists() {
        return Some(preferred);
    }
    let legacy = legacy_config_path(dir);
    if legacy.exists() {
        return Some(legacy);
    }
    None
}

fn add_child_to_config(
    parent_config_path: &Path,
    parent_dir: &Path,
    child_dir: &Path,
    child_name: &str,
    log: &mut String,
) -> Result<bool, InitError> {
    let mut cfg = Config::read_local(parent_config_path).map_err(InitError::SaveConfig)?;
    let rel_dir = pathdiff_rel(parent_dir, child_dir)
        .unwrap_or_else(|| child_dir.to_path_buf())
        .display()
        .to_string();

    // Prune stale children whose dirs no longer have an ax config.
    let mut pruned = false;
    let stale: Vec<String> = cfg
        .children
        .iter()
        .filter_map(|(name, entry)| {
            let resolved = if Path::new(&entry.dir).is_absolute() {
                PathBuf::from(&entry.dir)
            } else {
                parent_dir.join(&entry.dir)
            };
            if find_config_in_dir(&resolved).is_some() {
                None
            } else {
                Some(name.clone())
            }
        })
        .collect();
    for name in stale {
        if let Some(entry) = cfg.children.remove(&name) {
            pruned = true;
            let _ = writeln!(log, "Pruned stale child {name:?} -> {}", entry.dir);
        }
    }

    // Already pointing here?
    if cfg
        .children
        .values()
        .any(|existing| existing.dir == rel_dir)
    {
        if pruned {
            cfg.save(parent_config_path)
                .map_err(InitError::SaveConfig)?;
        }
        return Ok(false);
    }

    // Find a unique key.
    let mut entry_name = child_name.to_owned();
    let mut i = 2;
    while cfg.children.contains_key(&entry_name) {
        entry_name = format!("{child_name}-{i}");
        i += 1;
    }
    cfg.children.insert(
        entry_name,
        Child {
            dir: rel_dir,
            ..Default::default()
        },
    );
    cfg.save(parent_config_path)
        .map_err(InitError::SaveConfig)?;
    Ok(true)
}

fn pathdiff_rel(base: &Path, path: &Path) -> Option<PathBuf> {
    // Minimal path_diff: if path starts with base, strip it; otherwise return
    // path as-is so callers can fall back to the absolute form. This matches
    // the Go `filepath.Rel` behaviour in the common subpath case that
    // `ax init` relies on.
    let base_comps: Vec<_> = base.components().collect();
    let path_comps: Vec<_> = path.components().collect();
    if path_comps.len() < base_comps.len() {
        return None;
    }
    for (b, p) in base_comps.iter().zip(path_comps.iter()) {
        if b != p {
            return None;
        }
    }
    let mut out = PathBuf::new();
    for comp in &path_comps[base_comps.len()..] {
        out.push(comp);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Some(out)
}

fn ensure_gitignore(dir: &Path, pattern: &str) -> std::io::Result<bool> {
    let gitignore = dir.join(".gitignore");
    let existing = match fs::read_to_string(&gitignore) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if existing.is_none() && !dir.join(".git").exists() {
        return Ok(false);
    }
    let data = existing.unwrap_or_default();
    for line in data.lines() {
        if line.trim() == pattern {
            return Ok(false);
        }
    }
    let mut content = data;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(pattern);
    content.push('\n');
    fs::write(&gitignore, content)?;
    Ok(true)
}

fn refresh_orchestrator_tree(opts: &InitOptions, new_child_name: &str) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let Some(cfg_path) = ax_config::find_config_file(cwd) else {
        return Ok(());
    };
    let cfg = Config::load(&cfg_path).map_err(|e| e.to_string())?;
    let tree = Config::load_tree(&cfg_path).map_err(|e| e.to_string())?;
    let skip_root = cfg.disable_root_orchestrator;

    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ax-rs"));

    // When the daemon isn't running, just refresh artifacts.
    if !opts.daemon_running {
        ensure_orchestrator_tree(
            &RealTmux,
            &tree,
            &opts.socket_path,
            Some(&cfg_path),
            &current_exe,
            false,
            skip_root,
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    }

    ensure_orchestrator_tree(
        &RealTmux,
        &tree,
        &opts.socket_path,
        Some(&cfg_path),
        &current_exe,
        true,
        skip_root,
    )
    .map_err(|e| e.to_string())?;

    // Notify the root orchestrator so it can pick up the new child.
    let root_name = orchestrator_name(&tree.prefix);
    if ax_tmux::session_exists(&root_name) {
        if let Ok(mut client) = DaemonClient::connect(&opts.socket_path, "cli") {
            let msg = format!(
                "New sub-project `{new_child_name}` registered. Run list_agents/list_workspaces to see its workspaces and sub-orchestrator."
            );
            let _ = client.send_message(&root_name, &msg, None);
        }
    }
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn config_path_conflict(dir: &Path) -> Option<PathBuf> {
    let preferred = default_config_path(dir);
    if preferred.exists() {
        return Some(preferred);
    }
    let legacy = legacy_config_path(dir);
    if legacy.exists() {
        return Some(legacy);
    }
    None
}

fn which(name: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

#[derive(Debug)]
pub(crate) enum InitError {
    HomeDir,
    Cwd(std::io::Error),
    Io(std::io::Error),
    SaveConfig(ax_config::LoadError),
    SpawnAgent(std::io::Error),
    AgentExit { runtime: String, code: Option<i32> },
    LegacyConfigConflict(PathBuf),
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HomeDir => f.write_str("resolve home directory: HOME not set"),
            Self::Cwd(e) => write!(f, "resolve current directory: {e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::SaveConfig(e) => write!(f, "{e}"),
            Self::SpawnAgent(e) => write!(f, "spawn setup agent: {e}"),
            Self::AgentExit { runtime, code } => write!(
                f,
                "{runtime} setup agent exited with status {}",
                code.map_or_else(|| "signal".to_owned(), |c| c.to_string())
            ),
            Self::LegacyConfigConflict(p) => {
                write!(f, "legacy config already exists at {}", p.display())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn axis_parse_accepts_canonical_and_alias_forms() {
        assert_eq!(Axis::parse("auto"), Some(Axis::Auto));
        assert_eq!(Axis::parse("  ROLE  "), Some(Axis::Role));
        assert_eq!(Axis::parse("role-first"), Some(Axis::Role));
        assert_eq!(Axis::parse("domain"), Some(Axis::Domain));
        assert_eq!(Axis::parse("domain-first"), Some(Axis::Domain));
        assert_eq!(Axis::parse("nonsense"), None);
    }

    #[test]
    fn setup_prompt_embeds_conway_framing_and_axis_directive_for_auto() {
        let p = build_setup_system_prompt(Path::new("/tmp/.ax/config.yaml"), "codex", Axis::Auto);
        assert!(p.contains("Conway's Law"));
        assert!(p.contains("3~6개월"));
        assert!(p.contains("`auto`"));
        assert!(!p.contains("--axis role"));
        assert!(!p.contains("--axis domain"));
        assert!(p.contains("# axis:"));
    }

    #[test]
    fn setup_prompt_forces_role_when_role_axis_requested() {
        let p = build_setup_system_prompt(Path::new("/tmp/.ax/config.yaml"), "codex", Axis::Role);
        assert!(p.contains("--axis role"));
        assert!(!p.contains("`auto`\n관찰"));
        assert!(p.contains("역할 축으로 구성하세요"));
    }

    #[test]
    fn setup_prompt_forces_domain_when_domain_axis_requested() {
        let p = build_setup_system_prompt(Path::new("/tmp/.ax/config.yaml"), "codex", Axis::Domain);
        assert!(p.contains("--axis domain"));
        assert!(p.contains("도메인 이름"));
    }

    #[test]
    fn ensure_gitignore_creates_entry_when_git_dir_present() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let added = ensure_gitignore(tmp.path(), ".mcp.json").unwrap();
        assert!(added);
        let body = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(body.contains(".mcp.json"));
    }

    #[test]
    fn ensure_gitignore_noop_when_no_git_and_no_existing_file() {
        let tmp = TempDir::new().unwrap();
        let added = ensure_gitignore(tmp.path(), ".mcp.json").unwrap();
        assert!(!added);
        assert!(!tmp.path().join(".gitignore").exists());
    }

    #[test]
    fn ensure_gitignore_idempotent() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), ".mcp.json\n").unwrap();
        let added = ensure_gitignore(tmp.path(), ".mcp.json").unwrap();
        assert!(!added);
    }

    #[test]
    fn register_as_child_adds_child_entry_to_ancestor_config() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("parent");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();
        let parent_cfg = default_config_path(&parent);
        Config::default_for_runtime("parent", "claude")
            .save(&parent_cfg)
            .unwrap();
        // Write a minimal child config so the pruning pass considers it valid.
        let child_cfg = default_config_path(&child);
        Config::default_for_runtime("child", "claude")
            .save(&child_cfg)
            .unwrap();

        // Pass an explicit home override so the test never touches
        // the process-global HOME env — `std::env::set_var` races
        // with parallel tests on macOS.
        let mut log = String::new();
        let fake_home = tmp.path().join("no-home-here");
        let added = register_as_child_with_home(&child, "child", Some(fake_home), &mut log)
            .expect("register_as_child")
            .expect("parent path");
        assert_eq!(added, parent_cfg);
        let reloaded = Config::read_local(&parent_cfg).unwrap();
        assert!(reloaded.children.contains_key("child"));
        assert_eq!(reloaded.children.get("child").unwrap().dir, "child");
    }
}
