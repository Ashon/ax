//! Cross-crate safety-cap coverage: build real `.ax/config.yaml`
//! trees on disk and drive them through the public `Config::load`
//! path that CLI + daemon use at startup. Unit tests in ax-config
//! already cover the same logic — this file's purpose is to catch
//! regressions where a refactor breaks the path users actually hit
//! (path discovery, recursive child load, error surface).
//!
//! When we add runtime-side caps (concurrent agents, usage gating)
//! the matching e2e coverage belongs here too, booting an
//! in-process daemon against a config that trips each cap.

use std::fs;
use std::path::Path;

use ax_config::{default_config_path, Config, TreeError, ValidationError};

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn expect_validation(err: TreeError) -> ValidationError {
    let mut cur = err;
    loop {
        cur = match cur {
            TreeError::Validation(boxed) => return *boxed,
            TreeError::Child { source, .. } => *source,
            other => panic!("expected TreeError::Validation, got {other:?}"),
        };
    }
}

#[test]
fn depth_cap_fires_through_public_config_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    // 4 levels of children = depth 4; default cap is 3.
    let l1 = root.join("l1");
    let l2 = l1.join("l2");
    let l3 = l2.join("l3");
    let l4 = l3.join("l4");
    write(
        &default_config_path(root),
        "project: r\nchildren:\n  a:\n    dir: ./l1\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l1),
        "project: l1\nchildren:\n  b:\n    dir: ./l2\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l2),
        "project: l2\nchildren:\n  c:\n    dir: ./l3\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l3),
        "project: l3\nchildren:\n  d:\n    dir: ./l4\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );
    write(
        &default_config_path(&l4),
        "project: l4\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n",
    );

    let err = expect_validation(Config::load(default_config_path(root)).unwrap_err());
    assert!(
        matches!(err, ValidationError::OrchestratorDepthExceeded { depth: 4, cap: 3, .. }),
        "got {err:?}"
    );
}

mod capacity_fake {
    //! A minimal fake tmux backend for the capacity-cap e2e: lets us
    //! pretend N ax-managed sessions are already live so the spawn
    //! path sees the cap saturated and refuses a new one.

    use std::cell::Cell;
    use std::collections::BTreeMap;

    use ax_tmux::SessionInfo;
    use ax_workspace::TmuxBackend;

    #[derive(Default, Clone)]
    pub(super) struct CapFake {
        pub live: Cell<u32>,
    }

    impl TmuxBackend for CapFake {
        fn session_exists(&self, _workspace: &str) -> bool {
            false
        }
        fn list_sessions(&self) -> Result<Vec<SessionInfo>, ax_tmux::TmuxError> {
            Ok((0..self.live.get())
                .map(|i| SessionInfo {
                    name: ax_tmux::session_name(&format!("live{i}")),
                    workspace: format!("live{i}"),
                    attached: false,
                    windows: 1,
                })
                .collect())
        }
        fn is_idle(&self, _workspace: &str) -> bool {
            true
        }
        fn create_session(
            &self,
            _workspace: &str,
            _dir: &str,
            _shell: &str,
            _env: &BTreeMap<String, String>,
        ) -> Result<(), ax_tmux::TmuxError> {
            self.live.set(self.live.get() + 1);
            Ok(())
        }
        fn create_session_with_command(
            &self,
            _workspace: &str,
            _dir: &str,
            _command: &str,
            _env: &BTreeMap<String, String>,
        ) -> Result<(), ax_tmux::TmuxError> {
            self.live.set(self.live.get() + 1);
            Ok(())
        }
        fn create_session_with_args(
            &self,
            _workspace: &str,
            _dir: &str,
            _argv: &[String],
            _env: &BTreeMap<String, String>,
        ) -> Result<(), ax_tmux::TmuxError> {
            self.live.set(self.live.get() + 1);
            Ok(())
        }
        fn destroy_session(&self, _workspace: &str) -> Result<(), ax_tmux::TmuxError> {
            Ok(())
        }
    }
}

#[test]
fn concurrent_agent_cap_blocks_new_spawn_through_dispatch() {
    use capacity_fake::CapFake;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write(
        &default_config_path(root),
        "project: r\nmax_concurrent_agents: 2\nworkspaces:\n  w:\n    dir: ./w\n    runtime: claude\n",
    );
    // HOME must point somewhere writable because dispatch derives
    // the root-orchestrator artifact dir from $HOME/.ax.
    let home = tempfile::tempdir().expect("home");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: tests in this file run serially within the single-thread
    // default; no other thread reads HOME during the test body.
    unsafe { std::env::set_var("HOME", home.path()) };

    let tmux = CapFake::default();
    tmux.live.set(2);

    let result = ax_workspace::ensure_dispatch_target(
        &tmux,
        std::path::Path::new("/tmp/ax-e2e.sock"),
        &default_config_path(root),
        std::path::Path::new("/tmp/ax-e2e"),
        "w",
        false,
    );

    unsafe {
        if let Some(v) = prev_home {
            std::env::set_var("HOME", v);
        } else {
            std::env::remove_var("HOME");
        }
    }

    let err = result.expect_err("cap should block the spawn");
    let msg = err.to_string();
    assert!(
        msg.contains("max_concurrent_agents"),
        "expected cap-reached error, got {msg}"
    );
}

#[test]
fn children_cap_fires_through_public_config_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    // 3 children, cap lowered to 2.
    let mut yaml = String::from("project: r\nmax_children_per_node: 2\nchildren:\n");
    for name in ["k1", "k2", "k3"] {
        use std::fmt::Write as _;
        writeln!(yaml, "  {name}:\n    dir: ./{name}").unwrap();
        write(
            &default_config_path(root.join(name).as_path()),
            &format!("project: {name}\nworkspaces:\n  w:\n    dir: .\n    runtime: claude\n"),
        );
    }
    yaml.push_str("workspaces:\n  main:\n    dir: .\n    runtime: claude\n");
    write(&default_config_path(root), &yaml);

    let err = expect_validation(Config::load(default_config_path(root)).unwrap_err());
    assert!(
        matches!(err, ValidationError::ChildrenPerNodeExceeded { count: 3, cap: 2, .. }),
        "got {err:?}"
    );
}
