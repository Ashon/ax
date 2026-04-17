use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use ax_agent::{codex_home_path, Runtime};
use ax_config::Workspace;
use ax_workspace::{ensure_artifacts, managed_run_agent_args, Manager, TmuxBackend};

static HOME_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct FakeState {
    session_exists: Cell<bool>,
    calls: RefCell<Vec<String>>,
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

    fn calls(&self) -> Vec<String> {
        self.state.calls.borrow().clone()
    }

    fn argv(&self) -> Vec<String> {
        self.state.argv.borrow().clone()
    }
}

impl TmuxBackend for FakeTmux {
    fn session_exists(&self, _workspace: &str) -> bool {
        self.state.session_exists.get()
    }

    fn create_session(
        &self,
        _workspace: &str,
        dir: &str,
        _shell: &str,
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.calls.borrow_mut().push("create".to_owned());
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
        self.state
            .calls
            .borrow_mut()
            .push("create_command".to_owned());
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
        self.state.calls.borrow_mut().push("create_args".to_owned());
        *self.state.dir.borrow_mut() = Some(dir.to_owned());
        *self.state.argv.borrow_mut() = argv.to_vec();
        Ok(())
    }

    fn destroy_session(&self, _workspace: &str) -> Result<(), ax_tmux::TmuxError> {
        self.state.calls.borrow_mut().push("destroy".to_owned());
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
fn managed_run_agent_args_appends_fresh_flag() {
    let args = managed_run_agent_args(
        Path::new("/tmp/ax"),
        Runtime::Claude,
        "worker",
        Path::new("/tmp/ax.sock"),
        Some(Path::new("/tmp/ax.yaml")),
        true,
    );
    assert!(args.iter().any(|arg| arg == "--fresh"));
}

#[test]
fn manager_restart_removes_stale_codex_home_without_existing_session() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let socket_path = Path::new("/tmp/ax.sock");
        let config_path = home.path().join("project").join(".ax").join("config.yaml");
        let dir = home.path().join("project").join("worker");
        let workspace = Workspace {
            dir: dir.display().to_string(),
            runtime: "codex".to_owned(),
            instructions: "worker instructions".to_owned(),
            ..Workspace::default()
        };

        ensure_artifacts(
            "worker",
            &workspace,
            socket_path,
            Some(&config_path),
            Path::new("/tmp/ax"),
        )
        .unwrap();
        let codex_home = codex_home_path("worker", &workspace.dir).unwrap();
        let stale = codex_home.join("stale-session.txt");
        fs::write(&stale, "stale").unwrap();

        let tmux = FakeTmux::default();
        let manager = Manager::with_tmux(socket_path, Some(config_path), "/tmp/ax", tmux.clone());
        manager.restart("worker", &workspace).unwrap();

        assert_eq!(tmux.calls(), vec!["create_args".to_owned()]);
        assert!(!stale.exists());
        assert!(dir.join(".mcp.json").exists());
        assert!(codex_home.join("config.toml").exists());
    });
}

#[test]
fn manager_restart_destroys_existing_session_before_create() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let dir = home.path().join("project").join("worker");
        let workspace = Workspace {
            dir: dir.display().to_string(),
            runtime: "claude".to_owned(),
            ..Workspace::default()
        };

        let tmux = FakeTmux::default();
        tmux.set_session_exists(true);
        let manager = Manager::with_tmux(
            PathBuf::from("/tmp/ax.sock"),
            Some(home.path().join(".ax").join("config.yaml")),
            PathBuf::from("/tmp/ax"),
            tmux.clone(),
        );
        manager.restart("worker", &workspace).unwrap();

        assert_eq!(
            tmux.calls(),
            vec!["destroy".to_owned(), "create_args".to_owned()]
        );
        assert!(tmux.argv().iter().any(|arg| arg == "--fresh"));
    });
}
