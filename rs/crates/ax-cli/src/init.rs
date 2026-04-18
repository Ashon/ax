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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitOptions {
    pub global: bool,
    pub no_setup: bool,
    pub runtime: String,
    pub socket_path: PathBuf,
    pub daemon_running: bool,
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
        if let Err(e) = refresh_orchestrator_tree(opts, &project_name) {
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

    run_setup_agent(&dir, &path, &opts.runtime)?;
    Ok(String::new())
}

fn run_setup_agent(project_dir: &Path, config_path: &Path, runtime: &str) -> Result<(), InitError> {
    let system_prompt = build_setup_system_prompt(config_path, runtime);
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

fn build_setup_system_prompt(config_path: &Path, runtime: &str) -> String {
    let cp = config_path.display();
    format!(
        "당신은 ax 프로젝트 셋업 에이전트입니다. 사용자가 요청하면 현재 디렉토리의 프로젝트를 분석해서 멀티 에이전트 워크스페이스 구성을 제안하고 {cp} 파일을 편집하세요.\n\n\
## 절차\n\
1. 프로젝트 구조를 파악하세요 (Glob으로 디렉토리 구조, README/package.json/go.mod/pyproject.toml 등 주요 파일 확인).\n\
2. 모노레포인지, 어떤 도메인들이 있는지, 어떤 역할의 에이전트가 필요한지 판단하세요.\n\
3. **사용자에게 확인을 묻지 말고** 바로 {cp} 파일을 편집하세요.\n\
4. 편집 후 최종 구성을 요약해서 보여주고, 사용자가 조정을 요청하면 반영하세요.\n\n\
## config.yaml 형식\n\
```yaml\n\
project: <프로젝트 이름>\n\
workspaces:\n  \
<name>:\n    \
dir: <프로젝트 루트 기준 상대 경로>\n    \
description: <해당 에이전트의 역할 한 문장>\n    \
runtime: {runtime}\n    \
instructions: |\n      \
<해당 워크스페이스 에이전트가 받을 지침 — 무엇을 해야 하는지, 어떤 파일을 건드려야 하는지 등>\n\
```\n\n\
## 주의사항\n\
- 워크스페이스 이름은 kebab-case 또는 snake_case로 짧고 명확하게 (예: backend, frontend, infra, docs).\n\
- description은 한 문장으로 역할을 명확히 설명.\n\
- 이 초기화에서는 기본 런타임을 {runtime}로 사용하세요.\n\
- instructions는 구체적으로 작성 — 그 에이전트가 어떤 디렉토리에서 작업하고, 어떤 원칙을 따라야 하는지.\n\
- 기존 {cp} 파일은 최소 stub만 있는 상태입니다. workspaces 섹션을 채워주세요."
    )
}

fn register_as_child(
    child_dir: &Path,
    name: &str,
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
    if let Some(home) = home_dir() {
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

        std::env::set_var("HOME", "/tmp/definitely-not-an-ax-home-path");
        let mut log = String::new();
        let added = register_as_child(&child, "child", &mut log)
            .expect("register_as_child")
            .expect("parent path");
        assert_eq!(added, parent_cfg);
        let reloaded = Config::read_local(&parent_cfg).unwrap();
        assert!(reloaded.children.contains_key("child"));
        assert_eq!(reloaded.children.get("child").unwrap().dir, "child");
    }
}
