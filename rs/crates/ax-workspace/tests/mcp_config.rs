//! .mcp.json merge/write/remove behaviour pinned against the Go
//! workspace package.

use std::fs;
use std::path::{Path, PathBuf};

use ax_workspace::{remove_mcp_config, write_mcp_config, MCP_CONFIG_FILE};

fn ax_bin() -> PathBuf {
    PathBuf::from("/opt/ax/bin/ax")
}

fn read_json(path: &Path) -> serde_json::Value {
    let body = fs::read_to_string(path).unwrap();
    serde_json::from_str(&body).unwrap()
}

#[test]
fn write_creates_ax_entry_when_file_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let socket = PathBuf::from("/tmp/ax.sock");
    write_mcp_config(
        dir.path(),
        "worker",
        &socket,
        Some(Path::new("/etc/ax/config.yaml")),
        &ax_bin(),
    )
    .unwrap();

    let path = dir.path().join(MCP_CONFIG_FILE);
    let json = read_json(&path);
    let ax = &json["mcpServers"]["ax"];
    assert_eq!(ax["command"], "/opt/ax/bin/ax");
    let args: Vec<String> = ax["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        args,
        vec![
            "mcp-server",
            "--workspace",
            "worker",
            "--socket",
            "/tmp/ax.sock",
            "--config",
            "/etc/ax/config.yaml",
        ]
    );
    // Trailing newline per Go's behaviour.
    assert!(fs::read(&path).unwrap().ends_with(b"\n"));
}

#[test]
fn write_merges_into_existing_non_ax_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MCP_CONFIG_FILE);
    fs::write(
        &path,
        r#"{
  "mcpServers": {
    "other": {
      "command": "/usr/bin/other",
      "args": ["--flag"]
    }
  }
}
"#,
    )
    .unwrap();

    write_mcp_config(
        dir.path(),
        "worker",
        Path::new("/tmp/ax.sock"),
        None,
        &ax_bin(),
    )
    .unwrap();
    let json = read_json(&path);
    assert!(json["mcpServers"]["other"].is_object());
    assert!(json["mcpServers"]["ax"].is_object());
    // config_path absent → no --config flag emitted.
    let args: Vec<&str> = json["mcpServers"]["ax"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!args.contains(&"--config"));
}

#[test]
fn write_replaces_pre_existing_ax_entry_with_fresh_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MCP_CONFIG_FILE);
    fs::write(
        &path,
        r#"{"mcpServers":{"ax":{"command":"/old/ax","args":["stale"]}}}"#,
    )
    .unwrap();
    write_mcp_config(
        dir.path(),
        "worker",
        Path::new("/tmp/ax.sock"),
        None,
        &ax_bin(),
    )
    .unwrap();
    let json = read_json(&path);
    assert_eq!(json["mcpServers"]["ax"]["command"], "/opt/ax/bin/ax");
    assert_ne!(json["mcpServers"]["ax"]["args"][0], "stale");
}

#[test]
fn remove_deletes_file_when_ax_is_the_only_entry() {
    let dir = tempfile::tempdir().unwrap();
    write_mcp_config(
        dir.path(),
        "worker",
        Path::new("/tmp/ax.sock"),
        None,
        &ax_bin(),
    )
    .unwrap();
    let path = dir.path().join(MCP_CONFIG_FILE);
    assert!(path.exists());
    remove_mcp_config(dir.path()).unwrap();
    assert!(!path.exists());
}

#[test]
fn remove_leaves_sibling_entries_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MCP_CONFIG_FILE);
    fs::write(
        &path,
        r#"{"mcpServers":{"other":{"command":"/bin/o","args":[]},"ax":{"command":"/ax","args":[]}}}"#,
    )
    .unwrap();
    remove_mcp_config(dir.path()).unwrap();
    let json = read_json(&path);
    assert!(json["mcpServers"]["ax"].is_null());
    assert!(json["mcpServers"]["other"].is_object());
}

#[test]
fn remove_on_missing_file_is_no_op() {
    let dir = tempfile::tempdir().unwrap();
    remove_mcp_config(dir.path()).unwrap();
}
