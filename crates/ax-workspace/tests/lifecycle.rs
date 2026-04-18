use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use ax_agent::codex_home_path;
use ax_proto::types::LifecycleTargetKind;
use ax_tmux::SessionInfo;
use ax_workspace::{
    ensure_artifacts, restart_named_target, start_named_target, stop_named_target, TmuxBackend,
};

static HOME_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct FakeState {
    session_exists: Cell<bool>,
    destroyed: Cell<bool>,
    created_dir: RefCell<Option<String>>,
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

    fn created_dir(&self) -> Option<String> {
        self.state.created_dir.borrow().clone()
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
        self.state.session_exists.set(true);
        *self.state.created_dir.borrow_mut() = Some(dir.to_owned());
        Ok(())
    }

    fn create_session_with_command(
        &self,
        _workspace: &str,
        dir: &str,
        _command: &str,
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.session_exists.set(true);
        *self.state.created_dir.borrow_mut() = Some(dir.to_owned());
        Ok(())
    }

    fn create_session_with_args(
        &self,
        _workspace: &str,
        dir: &str,
        _argv: &[String],
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.session_exists.set(true);
        *self.state.created_dir.borrow_mut() = Some(dir.to_owned());
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

fn write_config(dir: &Path, content: &str) -> PathBuf {
    let path = dir.join(".ax").join("config.yaml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    path
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let is_absolute = path.is_absolute();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !is_absolute {
                    out.push("..");
                }
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    if out.as_os_str().is_empty() {
        if is_absolute {
            PathBuf::from("/")
        } else {
            PathBuf::from(".")
        }
    } else {
        out
    }
}

#[test]
fn stop_named_target_stops_workspace_without_deleting_artifacts() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let socket_path = Path::new("/tmp/ax.sock");
        let config_path = write_config(
            home.path(),
            "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: codex\n",
        );
        let dir = home.path().join("worker");

        ensure_artifacts(
            "worker",
            &ax_config::Workspace {
                dir: dir.display().to_string(),
                runtime: "codex".to_owned(),
                ..ax_config::Workspace::default()
            },
            socket_path,
            Some(&config_path),
            Path::new("/tmp/ax"),
        )
        .unwrap();
        let codex_home = codex_home_path("worker", &dir.display().to_string()).unwrap();
        std::fs::write(codex_home.join("keep-me.txt"), "persist").unwrap();

        let tmux = FakeTmux::default();
        tmux.set_session_exists(true);
        let target = stop_named_target(
            &tmux,
            socket_path,
            &config_path,
            Path::new("/tmp/ax"),
            "worker",
        )
        .unwrap();

        assert_eq!(target.kind, LifecycleTargetKind::Workspace);
        assert!(tmux.destroyed());
        assert!(dir.join(".mcp.json").exists());
        assert!(codex_home.join("keep-me.txt").exists());
    });
}

#[test]
fn start_named_target_starts_missing_workspace_by_exact_name() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = write_config(
            home.path(),
            "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let tmux = FakeTmux::default();

        let target = start_named_target(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "worker",
        )
        .unwrap();

        assert_eq!(target.kind, LifecycleTargetKind::Workspace);
        let actual = normalize(Path::new(
            tmux.created_dir()
                .as_deref()
                .expect("workspace create dir present"),
        ));
        assert_eq!(actual, home.path().join("worker"));
    });
}

#[test]
fn start_named_target_starts_managed_child_orchestrator() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let child_dir = home.path().join("child");
        let _child = write_config(
            &child_dir,
            "project: child\norchestrator_runtime: claude\nworkspaces:\n  dev:\n    dir: .\n    runtime: claude\n",
        );
        let config_path = write_config(
            home.path(),
            "project: root\nworkspaces:\n  root:\n    dir: .\n    runtime: claude\nchildren:\n  child:\n    dir: ./child\n    prefix: team\n",
        );
        let tmux = FakeTmux::default();

        let target = start_named_target(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "team.orchestrator",
        )
        .unwrap();

        assert_eq!(target.kind, LifecycleTargetKind::Orchestrator);
        let actual = normalize(Path::new(
            tmux.created_dir()
                .as_deref()
                .expect("orchestrator create dir present"),
        ));
        assert_eq!(actual, child_dir.join(".ax").join("orchestrator-team"));
    });
}

#[test]
fn start_named_target_rejects_root_orchestrator() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = write_config(
            home.path(),
            "project: root\norchestrator_runtime: claude\nworkspaces:\n  root:\n    dir: .\n    runtime: claude\n",
        );
        let tmux = FakeTmux::default();

        let err = start_named_target(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "orchestrator",
        )
        .unwrap_err();

        assert!(err.to_string().contains(
            "orchestrator \"orchestrator\" does not support targeted start because it is not a managed session"
        ));
    });
}

#[test]
fn restart_named_target_recycles_workspace_session() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = write_config(
            home.path(),
            "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let tmux = FakeTmux::default();
        tmux.set_session_exists(true);

        let target = restart_named_target(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "worker",
        )
        .unwrap();

        assert_eq!(target.kind, LifecycleTargetKind::Workspace);
        assert!(tmux.destroyed());
        let actual = normalize(Path::new(
            tmux.created_dir()
                .as_deref()
                .expect("workspace create dir present"),
        ));
        assert_eq!(actual, home.path().join("worker"));
    });
}
