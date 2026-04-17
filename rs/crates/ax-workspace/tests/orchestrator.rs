use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::Mutex;

use ax_agent::codex_home_path;
use ax_config::ProjectNode;
use ax_tmux::SessionInfo;
use ax_workspace::{
    cleanup_orchestrator_artifacts, cleanup_orchestrator_state, ensure_orchestrator,
    orchestrator_dir_for_node, orchestrator_name, root_orchestrator_dir, write_mcp_config,
    TmuxBackend,
};

static HOME_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct FakeState {
    session_exists: Cell<bool>,
    destroyed: Cell<bool>,
    created: Cell<bool>,
    argv: RefCell<Vec<String>>,
    dir: RefCell<Option<String>>,
}

#[derive(Clone, Default)]
struct FakeTmux {
    state: Rc<FakeState>,
}

impl FakeTmux {
    fn set_session_exists(&self, value: bool) {
        self.state.session_exists.set(value);
    }

    fn destroyed(&self) -> bool {
        self.state.destroyed.get()
    }

    fn created(&self) -> bool {
        self.state.created.get()
    }

    fn argv(&self) -> Vec<String> {
        self.state.argv.borrow().clone()
    }

    fn dir(&self) -> Option<String> {
        self.state.dir.borrow().clone()
    }
}

impl TmuxBackend for FakeTmux {
    fn session_exists(&self, _workspace: &str) -> bool {
        self.state.session_exists.get()
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, ax_tmux::TmuxError> {
        Ok(Vec::new())
    }

    fn is_idle(&self, _workspace: &str) -> bool {
        true
    }

    fn create_session(
        &self,
        _workspace: &str,
        dir: &str,
        _shell: &str,
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.created.set(true);
        self.state.session_exists.set(true);
        *self.state.dir.borrow_mut() = Some(dir.to_owned());
        Ok(())
    }

    fn create_session_with_command(
        &self,
        _workspace: &str,
        dir: &str,
        command: &str,
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.created.set(true);
        self.state.session_exists.set(true);
        *self.state.dir.borrow_mut() = Some(dir.to_owned());
        *self.state.argv.borrow_mut() = vec![command.to_owned()];
        Ok(())
    }

    fn create_session_with_args(
        &self,
        _workspace: &str,
        dir: &str,
        argv: &[String],
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.created.set(true);
        self.state.session_exists.set(true);
        *self.state.dir.borrow_mut() = Some(dir.to_owned());
        *self.state.argv.borrow_mut() = argv.to_vec();
        Ok(())
    }

    fn destroy_session(&self, _workspace: &str) -> Result<(), ax_tmux::TmuxError> {
        self.state.destroyed.set(true);
        self.state.session_exists.set(false);
        Ok(())
    }
}

fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = HOME_LOCK.lock().unwrap();
    let prev = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("HOME", home);
    }
    let out = f();
    if let Some(value) = prev {
        unsafe {
            std::env::set_var("HOME", value);
        }
    } else {
        unsafe {
            std::env::remove_var("HOME");
        }
    }
    out
}

#[test]
fn root_orchestrator_dir_uses_home_ax_orchestrator() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let dir = root_orchestrator_dir().unwrap();
        assert_eq!(dir, home.path().join(".ax").join("orchestrator"));
    });
}

#[test]
fn orchestrator_dir_for_node_uses_root_or_child_layout() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let root = ProjectNode {
            name: "root".to_owned(),
            dir: home.path().join("repo"),
            ..ProjectNode::default()
        };
        assert_eq!(
            orchestrator_dir_for_node(&root).unwrap(),
            home.path().join(".ax").join("orchestrator")
        );

        let child = ProjectNode {
            name: "child".to_owned(),
            prefix: "team.sub".to_owned(),
            dir: home.path().join("repo"),
            ..ProjectNode::default()
        };
        assert_eq!(
            orchestrator_dir_for_node(&child).unwrap(),
            home.path()
                .join("repo")
                .join(".ax")
                .join("orchestrator-team_sub")
        );
    });
}

#[test]
fn cleanup_orchestrator_artifacts_removes_generated_files_and_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let orch_dir = dir.path().join("orch");
    fs::create_dir_all(orch_dir.join(".claude")).unwrap();
    write_mcp_config(
        &orch_dir,
        "team.orchestrator",
        Path::new("/tmp/ax.sock"),
        Some(Path::new("/tmp/config.yaml")),
        Path::new("/tmp/ax"),
    )
    .unwrap();
    fs::write(orch_dir.join("CLAUDE.md"), "prompt").unwrap();
    fs::write(orch_dir.join("AGENTS.md"), "prompt").unwrap();

    cleanup_orchestrator_artifacts(&orch_dir).unwrap();
    assert!(!orch_dir.exists());
}

#[test]
fn cleanup_orchestrator_artifacts_keeps_dir_when_unrelated_files_remain() {
    let dir = tempfile::tempdir().unwrap();
    let orch_dir = dir.path().join("orch");
    fs::create_dir_all(orch_dir.join(".claude")).unwrap();
    write_mcp_config(
        &orch_dir,
        "team.orchestrator",
        Path::new("/tmp/ax.sock"),
        None,
        Path::new("/tmp/ax"),
    )
    .unwrap();
    fs::write(orch_dir.join("CLAUDE.md"), "prompt").unwrap();
    fs::write(orch_dir.join("keep.txt"), "keep").unwrap();

    cleanup_orchestrator_artifacts(&orch_dir).unwrap();
    assert!(orch_dir.exists());
    assert!(orch_dir.join("keep.txt").exists());
    assert!(!orch_dir.join(".mcp.json").exists());
    assert!(!orch_dir.join("CLAUDE.md").exists());
}

#[test]
fn cleanup_orchestrator_state_destroys_session_and_codex_home() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let name = orchestrator_name("team");
        let orch_dir = home
            .path()
            .join("repo")
            .join(".ax")
            .join("orchestrator-team");
        fs::create_dir_all(orch_dir.join(".claude")).unwrap();
        fs::write(orch_dir.join("AGENTS.md"), "prompt").unwrap();
        write_mcp_config(
            &orch_dir,
            &name,
            Path::new("/tmp/ax.sock"),
            None,
            Path::new("/tmp/ax"),
        )
        .unwrap();

        let codex_home = codex_home_path(&name, &orch_dir.display().to_string()).unwrap();
        fs::create_dir_all(&codex_home).unwrap();
        fs::write(codex_home.join("config.toml"), "config").unwrap();

        let tmux = FakeTmux::default();
        tmux.set_session_exists(true);
        cleanup_orchestrator_state(&tmux, &name, &orch_dir).unwrap();

        assert!(tmux.destroyed());
        assert!(!orch_dir.exists());
        assert!(!codex_home.exists());
    });
}

#[test]
fn ensure_root_orchestrator_writes_artifacts_without_starting_session() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let root = ProjectNode {
            name: "root".to_owned(),
            dir: home.path().join("repo"),
            ..ProjectNode::default()
        };

        let tmux = FakeTmux::default();
        ensure_orchestrator(
            &tmux,
            &root,
            "",
            Path::new("/tmp/ax.sock"),
            Some(Path::new("/tmp/config.yaml")),
            Path::new("/tmp/ax"),
            true,
        )
        .unwrap();

        let orch_dir = root_orchestrator_dir().unwrap();
        assert!(orch_dir.join(".mcp.json").exists());
        assert!(orch_dir.join(".claude").exists());
        assert!(orch_dir.join("CLAUDE.md").exists());
        assert!(!tmux.created());
    });
}

#[test]
fn ensure_sub_orchestrator_starts_missing_session_with_args() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let node = ProjectNode {
            name: "shared".to_owned(),
            alias: "alpha".to_owned(),
            prefix: "alpha".to_owned(),
            dir: home.path().join("repo"),
            orchestrator_runtime: "claude".to_owned(),
            ..ProjectNode::default()
        };

        let tmux = FakeTmux::default();
        ensure_orchestrator(
            &tmux,
            &node,
            "orchestrator",
            Path::new("/tmp/ax.sock"),
            Some(Path::new("/tmp/config.yaml")),
            Path::new("/tmp/ax"),
            true,
        )
        .unwrap();

        let orch_dir = orchestrator_dir_for_node(&node).unwrap();
        let orch_dir_string = orch_dir.display().to_string();
        let argv = tmux.argv();
        assert!(tmux.created());
        assert_eq!(tmux.dir().as_deref(), Some(orch_dir_string.as_str()));
        assert!(argv.iter().any(|arg| arg == "run-agent"));
        assert!(argv.iter().any(|arg| arg == "--runtime"));
        assert!(argv.iter().any(|arg| arg == "claude"));
        assert!(argv.iter().any(|arg| arg == "--workspace"));
        assert!(argv.iter().any(|arg| arg == "alpha.orchestrator"));
        assert!(orch_dir.join("CLAUDE.md").exists());
    });
}

#[test]
fn ensure_codex_orchestrator_prepares_codex_home() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let node = ProjectNode {
            name: "shared".to_owned(),
            prefix: "alpha".to_owned(),
            dir: home.path().join("repo"),
            orchestrator_runtime: "codex".to_owned(),
            ..ProjectNode::default()
        };

        let tmux = FakeTmux::default();
        ensure_orchestrator(
            &tmux,
            &node,
            "orchestrator",
            Path::new("/tmp/ax.sock"),
            Some(Path::new("/tmp/config.yaml")),
            Path::new("/tmp/ax"),
            false,
        )
        .unwrap();

        let name = orchestrator_name("alpha");
        let orch_dir = orchestrator_dir_for_node(&node).unwrap();
        let codex_home = codex_home_path(&name, &orch_dir.display().to_string()).unwrap();
        assert!(orch_dir.join("AGENTS.md").exists());
        assert!(codex_home.join("config.toml").exists());
        assert!(!tmux.created());
    });
}
