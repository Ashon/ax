//! ax-managed `CODEX_HOME` directory helpers.
//!
//! Every workspace running codex gets an isolated config dir under
//! `~/.ax/codex/<workspace>-<sha1>`. The sha1 is truncated to 6 bytes
//! (12 hex chars) and derived from the workspace's base directory — the
//! same truncation Go's `codexHomeKey` uses so a Rust daemon and a Go
//! daemon resolve the same path.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ax_config::{Config, DEFAULT_CODEX_REASONING_EFFORT};
use sha1::{Digest, Sha1};

#[derive(Debug, thiserror::Error)]
pub enum CodexHomeError {
    #[error("resolve home dir (HOME unset)")]
    HomeUnset,
    #[error("create codex home {path}: {source}")]
    CreateDir {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("read base codex config {path}: {source}")]
    ReadBaseConfig {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("link {dst} -> {src}: {source}")]
    Link {
        src: String,
        dst: String,
        #[source]
        source: io::Error,
    },
    #[error("write codex config {path}: {source}")]
    WriteConfig {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("remove codex home {path}: {source}")]
    Remove {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Stable per-workspace key used as the directory name. sha1(dir)[0..6]
/// hex-encoded, suffixed onto the workspace name.
#[must_use]
pub fn codex_home_key(workspace: &str, dir: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(dir.as_bytes());
    let digest = hasher.finalize();
    let truncated = &digest[..6];
    format!("{workspace}-{}", hex::encode(truncated))
}

/// Returns `$HOME/.ax/codex/<workspace>-<hash>` for the given workspace.
pub fn codex_home_path(workspace: &str, dir: &str) -> Result<PathBuf, CodexHomeError> {
    let home = resolve_home()?;
    Ok(home
        .join(".ax")
        .join("codex")
        .join(codex_home_key(workspace, dir)))
}

/// Create or refresh the ax-managed `CODEX_HOME` tree for a workspace,
/// including `auth.json` passthrough and an ax-specific `config.toml`.
pub fn prepare_codex_home(
    workspace: &str,
    dir: &str,
    socket_path: &Path,
    ax_bin: &Path,
    config_path: Option<&Path>,
) -> Result<PathBuf, CodexHomeError> {
    let home = resolve_home()?;
    let codex_home = codex_home_path(workspace, dir)?;
    fs::create_dir_all(&codex_home).map_err(|source| CodexHomeError::CreateDir {
        path: codex_home.display().to_string(),
        source,
    })?;

    link_if_present(
        &home.join(".codex").join("auth.json"),
        &codex_home.join("auth.json"),
    )?;

    let base_config = load_base_codex_config(&home.join(".codex").join("config.toml"))?;
    let reasoning_effort = resolve_codex_reasoning_effort(config_path, workspace)?;

    let mut content = upsert_top_level_key(
        &base_config,
        "model_reasoning_effort",
        &toml_quote(&reasoning_effort),
    );
    content = upsert_key_in_section(
        &content,
        &format!("[projects.{}]", toml_quote(dir)),
        "trust_level",
        "\"trusted\"",
    );

    let mut args = vec![
        "mcp-server".to_owned(),
        "--workspace".to_owned(),
        workspace.to_owned(),
        "--socket".to_owned(),
        socket_path.display().to_string(),
    ];
    if let Some(path) = config_path {
        let rendered = path.display().to_string();
        if !rendered.is_empty() {
            args.push("--config".to_owned());
            args.push(rendered);
        }
    }
    content = upsert_key_in_section(
        &content,
        "[mcp_servers.ax]",
        "command",
        &toml_quote(&ax_bin.display().to_string()),
    );
    content = upsert_key_in_section(&content, "[mcp_servers.ax]", "args", &toml_array(&args));

    let config_toml = codex_home.join("config.toml");
    fs::write(&config_toml, ensure_trailing_newline(&content)).map_err(|source| {
        CodexHomeError::WriteConfig {
            path: config_toml.display().to_string(),
            source,
        }
    })?;

    Ok(codex_home)
}

/// Like [`prepare_codex_home`] but removes the previous managed tree
/// before regenerating it when `fresh` is true.
pub fn prepare_codex_home_for_launch(
    workspace: &str,
    dir: &str,
    socket_path: &Path,
    ax_bin: &Path,
    config_path: Option<&Path>,
    fresh: bool,
) -> Result<PathBuf, CodexHomeError> {
    if fresh {
        remove_codex_home(workspace, dir)?;
    }
    prepare_codex_home(workspace, dir, socket_path, ax_bin, config_path)
}

/// Delete the managed `CODEX_HOME` directory for a workspace. Silently
/// succeeds when the directory doesn't exist.
pub fn remove_codex_home(workspace: &str, dir: &str) -> Result<(), CodexHomeError> {
    let path = codex_home_path(workspace, dir)?;
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CodexHomeError::Remove {
            path: path.display().to_string(),
            source: e,
        }),
    }
}

fn resolve_home() -> Result<PathBuf, CodexHomeError> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(CodexHomeError::HomeUnset)
}

/// Exposed for integration tests elsewhere in the workspace (mostly
/// `ax-usage`): check whether a path looks like it sits under an
/// ax-managed codex home.
#[must_use]
pub fn is_managed_codex_home(path: &Path) -> bool {
    path.ancestors().any(|p| {
        p.file_name()
            .and_then(|os| os.to_str())
            .is_some_and(|s| s == "codex")
            && p.parent()
                .and_then(Path::file_name)
                .and_then(|os| os.to_str())
                == Some(".ax")
    })
}

fn load_base_codex_config(path: &Path) -> Result<String, CodexHomeError> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(CodexHomeError::ReadBaseConfig {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn link_if_present(src: &Path, dst: &Path) -> Result<(), CodexHomeError> {
    let Ok(meta) = fs::symlink_metadata(src) else {
        return Ok(());
    };
    if meta.is_dir() {
        return Ok(());
    }

    if let Ok(existing) = fs::symlink_metadata(dst) {
        if existing.file_type().is_symlink()
            && fs::read_link(dst)
                .ok()
                .as_deref()
                .is_some_and(|target| target == src)
        {
            return Ok(());
        }

        if existing.is_dir() {
            fs::remove_dir_all(dst).map_err(|source| CodexHomeError::Link {
                src: src.display().to_string(),
                dst: dst.display().to_string(),
                source,
            })?;
        } else {
            fs::remove_file(dst).map_err(|source| CodexHomeError::Link {
                src: src.display().to_string(),
                dst: dst.display().to_string(),
                source,
            })?;
        }
    }

    create_symlink(src, dst).map_err(|source| CodexHomeError::Link {
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        source,
    })
}

#[cfg(unix)]
fn create_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn create_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

fn resolve_codex_reasoning_effort(
    config_path: Option<&Path>,
    workspace: &str,
) -> Result<String, CodexHomeError> {
    let Some(path) = config_path else {
        return Ok(DEFAULT_CODEX_REASONING_EFFORT.to_owned());
    };
    if path.as_os_str().is_empty() {
        return Ok(DEFAULT_CODEX_REASONING_EFFORT.to_owned());
    }

    match Config::load(path) {
        Ok(cfg) => Ok(codex_reasoning_effort_for_workspace(&cfg, workspace)),
        Err(_) => Ok(DEFAULT_CODEX_REASONING_EFFORT.to_owned()),
    }
}

fn codex_reasoning_effort_for_workspace(cfg: &Config, workspace: &str) -> String {
    if let Some(ws) = cfg.workspaces.get(workspace) {
        let trimmed = ws.codex_model_reasoning_effort.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    let trimmed = cfg.codex_model_reasoning_effort.trim();
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }
    DEFAULT_CODEX_REASONING_EFFORT.to_owned()
}

fn upsert_key_in_section(content: &str, header: &str, key: &str, value: &str) -> String {
    let mut lines = split_lines(content);
    let mut section_start = None;
    let mut section_end = lines.len();

    for (idx, line) in lines.iter().enumerate() {
        if line.trim() == header {
            section_start = Some(idx);
            for (inner_idx, inner) in lines.iter().enumerate().skip(idx + 1) {
                let trimmed = inner.trim();
                if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    section_end = inner_idx;
                    break;
                }
            }
            break;
        }
    }

    let entry = format!("{key} = {value}");
    let Some(start) = section_start else {
        if lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(header.to_owned());
        lines.push(entry);
        return lines.join("\n");
    };

    for line in lines.iter_mut().take(section_end).skip(start + 1) {
        let trimmed = line.trim();
        if trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")) {
            *line = entry;
            return lines.join("\n");
        }
    }

    lines.insert(section_end, entry);
    lines.join("\n")
}

fn upsert_top_level_key(content: &str, key: &str, value: &str) -> String {
    let mut lines = split_lines(content);
    let entry = format!("{key} = {value}");

    for idx in 0..lines.len() {
        let trimmed = lines[idx].trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            lines.insert(idx, entry);
            return lines.join("\n");
        }
        if trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")) {
            lines[idx] = entry;
            return lines.join("\n");
        }
    }

    lines.push(entry);
    lines.join("\n")
}

fn split_lines(content: &str) -> Vec<String> {
    let trimmed = content.trim_end_matches('\n');
    if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.lines().map(ToOwned::to_owned).collect()
    }
}

fn ensure_trailing_newline(content: &str) -> String {
    if content.is_empty() || content.ends_with('\n') {
        content.to_owned()
    } else {
        format!("{content}\n")
    }
}

fn toml_quote(value: &str) -> String {
    format!("{value:?}")
}

fn toml_array(values: &[String]) -> String {
    let quoted: Vec<String> = values.iter().map(|value| toml_quote(value)).collect();
    format!("[{}]", quoted.join(","))
}

#[cfg(test)]
mod tests {
    use super::upsert_top_level_key;

    #[test]
    fn upsert_top_level_key_replaces_existing_value() {
        let content = "model = \"gpt-5.4\"\nmodel_reasoning_effort = \"medium\"\n[projects.\"/tmp/demo\"]\ntrust_level = \"trusted\"\n";
        let updated = upsert_top_level_key(content, "model_reasoning_effort", "\"xhigh\"");
        assert_eq!(
            updated
                .matches("model_reasoning_effort = \"xhigh\"")
                .count(),
            1
        );
        assert!(!updated.contains("model_reasoning_effort = \"medium\""));
    }
}
