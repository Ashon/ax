use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use ax_tmux::SessionInfo;
use ax_workspace::{
    dispatch_runnable_work_with_options, ensure_dispatch_target, DispatchBackend, DispatchOptions,
    TmuxBackend,
};

static HOME_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct FakeState {
    session_exists: Cell<bool>,
    idle_after_checks: Cell<i32>,
    idle_checks: Cell<i32>,
    wake_prompt: RefCell<String>,
    created_dir: RefCell<Option<String>>,
    created_args: RefCell<Vec<String>>,
    woke: Cell<bool>,
    /// Extra phantom ax-managed sessions reported by `list_sessions`.
    /// Used by the capacity-cap tests to simulate a full host.
    extra_sessions: Cell<u32>,
}

#[derive(Clone, Default)]
struct FakeTmux {
    state: Rc<FakeState>,
}

impl FakeTmux {
    fn with_idle_after_checks(checks: i32) -> Self {
        let tmux = Self::default();
        tmux.state.idle_after_checks.set(checks);
        tmux
    }

    fn idle_checks(&self) -> i32 {
        self.state.idle_checks.get()
    }

    fn created_dir(&self) -> Option<String> {
        self.state.created_dir.borrow().clone()
    }
    fn wake_prompt(&self) -> String {
        self.state.wake_prompt.borrow().clone()
    }

    fn woke(&self) -> bool {
        self.state.woke.get()
    }
}

impl TmuxBackend for FakeTmux {
    fn session_exists(&self, _workspace: &str) -> bool {
        self.state.session_exists.get()
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, ax_tmux::TmuxError> {
        let mut out = Vec::new();
        if self.state.session_exists.get() {
            out.push(SessionInfo {
                name: ax_tmux::session_name("worker"),
                workspace: "worker".to_owned(),
                attached: false,
                windows: 1,
            });
        }
        for i in 0..self.state.extra_sessions.get() {
            let ws = format!("phantom{i}");
            out.push(SessionInfo {
                name: ax_tmux::session_name(&ws),
                workspace: ws,
                attached: false,
                windows: 1,
            });
        }
        Ok(out)
    }

    fn is_idle(&self, _workspace: &str) -> bool {
        let next = self.state.idle_checks.get() + 1;
        self.state.idle_checks.set(next);
        next >= self.state.idle_after_checks.get()
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
        command: &str,
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.session_exists.set(true);
        *self.state.created_dir.borrow_mut() = Some(dir.to_owned());
        *self.state.created_args.borrow_mut() = vec![command.to_owned()];
        Ok(())
    }

    fn create_session_with_args(
        &self,
        _workspace: &str,
        dir: &str,
        argv: &[String],
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.session_exists.set(true);
        *self.state.created_dir.borrow_mut() = Some(dir.to_owned());
        *self.state.created_args.borrow_mut() = argv.to_vec();
        Ok(())
    }

    fn destroy_session(&self, _workspace: &str) -> Result<(), ax_tmux::TmuxError> {
        self.state.session_exists.set(false);
        Ok(())
    }
}

impl DispatchBackend for FakeTmux {
    fn wake_workspace(&self, _workspace: &str, prompt: &str) -> Result<(), ax_tmux::TmuxError> {
        self.state.woke.set(true);
        prompt.clone_into(&mut self.state.wake_prompt.borrow_mut());
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
fn ensure_dispatch_target_creates_missing_workspace_session() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = write_config(
            home.path(),
            "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let tmux = FakeTmux::default();

        ensure_dispatch_target(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "worker",
            false,
        )
        .unwrap();

        let actual = normalize(Path::new(
            tmux.created_dir()
                .as_deref()
                .expect("workspace create dir present"),
        ));
        assert_eq!(actual, home.path().join("worker"));
    });
}

#[test]
fn ensure_dispatch_target_rejects_missing_root_orchestrator_session() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = write_config(
            home.path(),
            "project: root\norchestrator_runtime: claude\nworkspaces:\n  root:\n    dir: .\n    runtime: claude\n",
        );
        let tmux = FakeTmux::default();

        let err = ensure_dispatch_target(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "orchestrator",
            false,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("is not running and is not a managed session"));
    });
}

#[test]
fn ensure_dispatch_target_blocks_new_spawn_past_capacity_cap() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        // Cap at 2 live ax sessions. Pre-populate 2 phantoms so a
        // new spawn would push the count to 3.
        let config_path = write_config(
            home.path(),
            "project: root\nmax_concurrent_agents: 2\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let tmux = FakeTmux::default();
        tmux.state.extra_sessions.set(2);

        let err = ensure_dispatch_target(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "worker",
            false,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_concurrent_agents"),
            "expected cap message, got {msg}"
        );
        // Session was not created (no dir recorded).
        assert!(tmux.created_dir().is_none());
    });
}

#[test]
fn ensure_dispatch_target_skips_cap_check_when_session_already_live() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        // Cap at 1 but the target is already live; early return
        // should skip the cap check (no-op idempotent call).
        let config_path = write_config(
            home.path(),
            "project: root\nmax_concurrent_agents: 1\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let tmux = FakeTmux::default();
        tmux.state.session_exists.set(true);
        tmux.state.extra_sessions.set(5);

        ensure_dispatch_target(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "worker",
            false,
        )
        .expect("already-live target short-circuits before the cap check");
    });
}

#[test]
fn dispatch_runnable_work_waits_for_new_session_then_wakes() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = write_config(
            home.path(),
            "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let tmux = FakeTmux::with_idle_after_checks(3);

        dispatch_runnable_work_with_options(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "worker",
            "ax.orchestrator",
            false,
            DispatchOptions {
                ready_timeout: Duration::from_millis(5),
                ready_poll_interval: Duration::ZERO,
                ready_settle_delay: Duration::ZERO,
                ready_fallback_delay: Duration::ZERO,
            },
        )
        .unwrap();

        assert!(tmux.woke());
        assert!(tmux.idle_checks() >= 3);
        let prompt = tmux.wake_prompt();
        for want in [
            "`read_messages`로 확인",
            "`list_tasks(assignee=<self>, status=\"pending\")`",
            "`list_tasks(assignee=<self>, status=\"in_progress\")`",
            "`get_task`로 구조화된 문맥",
            "지원되는 `send_message` 대상임이 확실하면",
            "send_message(to=\"ax.orchestrator\")",
        ] {
            assert!(prompt.contains(want), "missing {want:?} in:\n{prompt}");
        }
    });
}

#[test]
fn dispatch_wake_prompt_qualifies_cli_reply_path() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = write_config(
            home.path(),
            "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let tmux = FakeTmux::with_idle_after_checks(1);

        dispatch_runnable_work_with_options(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "worker",
            "_cli",
            false,
            DispatchOptions {
                ready_timeout: Duration::from_millis(5),
                ready_poll_interval: Duration::ZERO,
                ready_settle_delay: Duration::ZERO,
                ready_fallback_delay: Duration::ZERO,
            },
        )
        .unwrap();

        let prompt = tmux.wake_prompt();
        assert!(prompt.contains("send_message(to=\"_cli\")"));
        assert!(prompt.contains("지원되는 `send_message` 대상임이 확실하면"));
        assert!(prompt.contains("현재 최종 응답 또는 지원되는 상위 reply path"));
    });
}

#[test]
fn dispatch_existing_session_skips_startup_wait() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = write_config(
            home.path(),
            "project: root\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let tmux = FakeTmux::with_idle_after_checks(100);
        tmux.state.session_exists.set(true);

        dispatch_runnable_work_with_options(
            &tmux,
            Path::new("/tmp/ax.sock"),
            &config_path,
            Path::new("/tmp/ax"),
            "worker",
            "ax.orchestrator",
            false,
            DispatchOptions {
                ready_timeout: Duration::from_millis(5),
                ready_poll_interval: Duration::ZERO,
                ready_settle_delay: Duration::ZERO,
                ready_fallback_delay: Duration::ZERO,
            },
        )
        .unwrap();

        assert_eq!(tmux.idle_checks(), 0);
    });
}
