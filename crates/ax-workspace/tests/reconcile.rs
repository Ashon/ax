use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use ax_agent::codex_home_path;
use ax_config::{ProjectNode, Workspace};
use ax_tmux::SessionInfo;
use ax_workspace::{
    build_desired_state, build_desired_state_with_tree, ensure_artifacts,
    orchestrator_dir_for_node, root_orchestrator_dir, DesiredState, DesiredWorkspace,
    ReconcileOptions, Reconciler, TmuxBackend,
};

static HOME_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct FakeState {
    session_exists: Cell<bool>,
    idle: Cell<bool>,
    sessions: Vec<SessionInfo>,
}

#[derive(Clone, Default)]
struct FakeTmux {
    state: Rc<FakeState>,
}

impl FakeTmux {
    fn with_sessions(sessions: Vec<SessionInfo>, idle: bool) -> Self {
        Self {
            state: Rc::new(FakeState {
                session_exists: Cell::new(!sessions.is_empty()),
                idle: Cell::new(idle),
                sessions,
            }),
        }
    }
}

impl TmuxBackend for FakeTmux {
    fn session_exists(&self, _workspace: &str) -> bool {
        self.state.session_exists.get()
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, ax_tmux::TmuxError> {
        Ok(self.state.sessions.clone())
    }

    fn is_idle(&self, _workspace: &str) -> bool {
        self.state.idle.get()
    }

    fn create_session(
        &self,
        _workspace: &str,
        _dir: &str,
        _shell: &str,
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.session_exists.set(true);
        Ok(())
    }

    fn create_session_with_command(
        &self,
        _workspace: &str,
        _dir: &str,
        _command: &str,
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.session_exists.set(true);
        Ok(())
    }

    fn create_session_with_args(
        &self,
        _workspace: &str,
        _dir: &str,
        _argv: &[String],
        _env: &BTreeMap<String, String>,
    ) -> Result<(), ax_tmux::TmuxError> {
        self.state.session_exists.set(true);
        Ok(())
    }

    fn destroy_session(&self, _workspace: &str) -> Result<(), ax_tmux::TmuxError> {
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
fn build_desired_state_copies_workspaces_from_config() {
    let mut cfg = ax_config::Config::default_for_runtime("demo", "claude");
    cfg.workspaces.insert(
        "worker".to_owned(),
        Workspace {
            dir: "worker".to_owned(),
            runtime: "codex".to_owned(),
            ..Workspace::default()
        },
    );
    let desired = build_desired_state(&cfg, "/tmp/ax.sock", "/tmp/demo/.ax/config.yaml");
    assert!(desired.workspaces.contains_key("main"));
    assert!(desired.workspaces.contains_key("worker"));
}

#[test]
fn build_desired_state_with_tree_includes_root_and_child_orchestrators() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let cfg = ax_config::Config::default_for_runtime("demo", "claude");
        let tree = ProjectNode {
            name: "root".to_owned(),
            dir: home.path().join("project"),
            children: vec![ProjectNode {
                name: "shared".to_owned(),
                alias: "alpha".to_owned(),
                prefix: "alpha".to_owned(),
                dir: home.path().join("project").join("shared"),
                orchestrator_runtime: "codex".to_owned(),
                ..ProjectNode::default()
            }],
            ..ProjectNode::default()
        };

        let desired = build_desired_state_with_tree(
            &cfg,
            &tree,
            "/tmp/ax.sock",
            "/tmp/demo/.ax/config.yaml",
            true,
        )
        .unwrap();

        assert!(desired.orchestrators.contains_key("orchestrator"));
        assert!(desired.orchestrators.contains_key("alpha.orchestrator"));
        assert_eq!(
            desired
                .orchestrators
                .get("alpha.orchestrator")
                .expect("child orchestrator present")
                .parent_name,
            "orchestrator"
        );
    });
}

#[test]
fn reconcile_desired_state_creates_and_cleans_generated_artifacts() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = home.path().join("project").join(".ax").join("config.yaml");
        let socket_path = PathBuf::from("/tmp/ax.sock");
        let ax_bin = PathBuf::from("/tmp/ax");
        let tmux = FakeTmux::default();
        let reconciler = Reconciler::with_tmux(
            socket_path.clone(),
            config_path.clone(),
            ax_bin.clone(),
            tmux,
        );

        let old_workspace_dir = home.path().join("project").join("old");
        ensure_artifacts(
            "old",
            &Workspace {
                dir: old_workspace_dir.display().to_string(),
                runtime: "codex".to_owned(),
                instructions: "old workspace instructions".to_owned(),
                ..Workspace::default()
            },
            &socket_path,
            Some(&config_path),
            &ax_bin,
        )
        .unwrap();
        let old_codex_home =
            codex_home_path("old", &old_workspace_dir.display().to_string()).unwrap();

        let new_workspace_dir = home.path().join("project").join("new");
        let mut desired = DesiredState {
            socket_path: socket_path.clone(),
            config_path: config_path.clone(),
            workspaces: BTreeMap::new(),
            orchestrators: BTreeMap::new(),
            max_concurrent_agents: 0,
        };
        desired.workspaces.insert(
            "new".to_owned(),
            DesiredWorkspace {
                name: "new".to_owned(),
                workspace: Workspace {
                    dir: new_workspace_dir.display().to_string(),
                    runtime: "claude".to_owned(),
                    instructions: "new workspace instructions".to_owned(),
                    ..Workspace::default()
                },
            },
        );

        let mut previous = DesiredState {
            socket_path,
            config_path: config_path.clone(),
            workspaces: BTreeMap::new(),
            orchestrators: BTreeMap::new(),
            max_concurrent_agents: 0,
        };
        previous.workspaces.insert(
            "old".to_owned(),
            DesiredWorkspace {
                name: "old".to_owned(),
                workspace: Workspace {
                    dir: old_workspace_dir.display().to_string(),
                    runtime: "codex".to_owned(),
                    instructions: "old".to_owned(),
                    ..Workspace::default()
                },
            },
        );
        let previous_reconciler = Reconciler::with_tmux(
            "/tmp/ax.sock",
            config_path.clone(),
            ax_bin.clone(),
            FakeTmux::default(),
        );
        previous_reconciler
            .reconcile_desired_state(&previous, ReconcileOptions::default())
            .unwrap();

        let report = reconciler
            .reconcile_desired_state(&desired, ReconcileOptions::default())
            .unwrap();
        assert!(report
            .actions
            .iter()
            .any(|action| action.name == "new" && action.operation == "create"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.name == "old" && action.operation == "remove"));

        assert!(!old_workspace_dir.join(".mcp.json").exists());
        assert!(!old_workspace_dir.join("AGENTS.md").exists());
        assert!(!old_codex_home.exists());
        assert!(new_workspace_dir.join(".mcp.json").exists());
        assert!(new_workspace_dir.join("CLAUDE.md").exists());
    });
}

#[test]
fn reconcile_desired_state_blocks_busy_workspace_restart() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = home.path().join("project").join(".ax").join("config.yaml");
        let ax_bin = PathBuf::from("/tmp/ax");

        let mut previous = DesiredState {
            socket_path: PathBuf::from("/tmp/ax.sock"),
            config_path: config_path.clone(),
            workspaces: BTreeMap::new(),
            orchestrators: BTreeMap::new(),
            max_concurrent_agents: 0,
        };
        previous.workspaces.insert(
            "alpha".to_owned(),
            DesiredWorkspace {
                name: "alpha".to_owned(),
                workspace: Workspace {
                    dir: home
                        .path()
                        .join("project")
                        .join("alpha")
                        .display()
                        .to_string(),
                    runtime: "claude".to_owned(),
                    instructions: "old instructions".to_owned(),
                    ..Workspace::default()
                },
            },
        );
        let previous_reconciler = Reconciler::with_tmux(
            "/tmp/ax.sock",
            config_path.clone(),
            ax_bin.clone(),
            FakeTmux::default(),
        );
        previous_reconciler
            .reconcile_desired_state(&previous, ReconcileOptions::default())
            .unwrap();

        let tmux = FakeTmux::with_sessions(
            vec![SessionInfo {
                name: ax_tmux::session_name("alpha"),
                workspace: "alpha".to_owned(),
                attached: false,
                windows: 1,
            }],
            false,
        );
        let reconciler = Reconciler::with_tmux("/tmp/ax.sock", config_path.clone(), ax_bin, tmux);

        let mut desired = DesiredState {
            socket_path: PathBuf::from("/tmp/ax.sock"),
            config_path,
            workspaces: BTreeMap::new(),
            orchestrators: BTreeMap::new(),
            max_concurrent_agents: 0,
        };
        desired.workspaces.insert(
            "alpha".to_owned(),
            DesiredWorkspace {
                name: "alpha".to_owned(),
                workspace: Workspace {
                    dir: home
                        .path()
                        .join("project")
                        .join("alpha")
                        .display()
                        .to_string(),
                    runtime: "claude".to_owned(),
                    instructions: "new instructions".to_owned(),
                    ..Workspace::default()
                },
            },
        );

        let report = reconciler
            .reconcile_desired_state(
                &desired,
                ReconcileOptions {
                    daemon_running: true,
                    allow_disruptive_changes: false,
                },
            )
            .unwrap();

        assert!(report.actions.iter().any(|action| {
            action.kind == "workspace"
                && action.name == "alpha"
                && action.operation == "blocked_restart"
        }));
    });
}

#[test]
fn reconcile_desired_state_creates_orchestrator_artifacts_and_flags_root_restart() {
    let home = tempfile::tempdir().unwrap();
    with_home(home.path(), || {
        let config_path = home.path().join("project").join(".ax").join("config.yaml");
        let ax_bin = PathBuf::from("/tmp/ax");
        let cfg = ax_config::Config {
            project: "demo".to_owned(),
            ..ax_config::Config::default()
        };
        let tree = ProjectNode {
            name: "root".to_owned(),
            dir: home.path().join("project"),
            children: vec![ProjectNode {
                name: "shared".to_owned(),
                alias: "alpha".to_owned(),
                prefix: "alpha".to_owned(),
                dir: home.path().join("project").join("shared"),
                orchestrator_runtime: "claude".to_owned(),
                ..ProjectNode::default()
            }],
            ..ProjectNode::default()
        };
        let desired =
            build_desired_state_with_tree(&cfg, &tree, "/tmp/ax.sock", &config_path, true).unwrap();

        let reconciler = Reconciler::with_tmux(
            "/tmp/ax.sock",
            config_path.clone(),
            ax_bin,
            FakeTmux::default(),
        );
        let report = reconciler
            .reconcile_desired_state(&desired, ReconcileOptions::default())
            .unwrap();

        assert!(report.root_manual_restart_required);
        assert!(report.actions.iter().any(|action| {
            action.kind == "orchestrator"
                && action.name == "orchestrator"
                && action.operation == "create_artifacts"
        }));
        assert!(report.actions.iter().any(|action| {
            action.kind == "orchestrator"
                && action.name == "alpha.orchestrator"
                && action.operation == "create"
        }));

        let root_dir = root_orchestrator_dir().unwrap();
        let child_dir = orchestrator_dir_for_node(
            &tree
                .children
                .first()
                .expect("child project present")
                .clone(),
        )
        .unwrap();
        assert!(root_dir.join(".mcp.json").exists());
        assert!(root_dir.join("CLAUDE.md").exists());
        assert!(child_dir.join(".mcp.json").exists());
        assert!(child_dir.join("CLAUDE.md").exists());
    });
}
