//! Persistent task store for the daemon. The JSON on disk is a sorted
//! `Vec<Task>`, derived fields (`sequence`, `stale_info`) are stripped
//! before persistence, and every mutation refreshes the parent rollup
//! in place.
//!
//! The store is intentionally free of queue / wake-scheduler /
//! session-manager coupling; those orchestrations live in the
//! envelope handlers so the pure state transitions can be tested in
//! isolation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use ax_proto::types::{
    Task, TaskLog, TaskPriority, TaskRollup, TaskStartMode, TaskStatus, TaskWorkflowMode,
};

use crate::atomicfile::write_file_atomic;
use crate::task_helpers::{
    format_task_dispatch_message, has_concrete_evidence, looks_like_noop_status_message,
    normalize_message_for_suppression, DUPLICATE_SUPPRESSION_WINDOW, OPERATOR_WORKSPACE_NAME,
};

pub(crate) const STATE_FILE: &str = "tasks-state.json";
pub(crate) const SNAPSHOT_FILE: &str = "tasks.json";

#[derive(Debug, thiserror::Error)]
pub enum TaskStoreError {
    #[error("parent task {0:?} not found")]
    ParentNotFound(String),
    #[error("parent task {0:?} has been removed")]
    ParentRemoved(String),
    #[error("task {0:?} not found")]
    NotFound(String),
    #[error("task {0:?} has been removed")]
    Removed(String),
    #[error("workspace {0:?} cannot update task {1:?}")]
    UnauthorisedUpdate(String, String),
    #[error("workspace {0:?} cannot set result for task {1:?} owned by {2:?}")]
    UnauthorisedResult(String, String, String),
    #[error("workspace {0:?} cannot change status for task {1:?} owned by {2:?}")]
    UnauthorisedStatus(String, String, String),
    #[error("invalid task status transition {0:?} -> {1:?}")]
    InvalidTransition(String, String),
    #[error("task {0:?} version mismatch: have {1} want {2}")]
    VersionMismatch(String, i64, i64),
    #[error("workspace {0:?} cannot manage task {1:?}")]
    UnauthorisedControl(String, String),
    #[error("task {0:?} is not pending/in_progress/blocked")]
    NotRetryable(String),
    #[error("task {0:?} is already terminal ({1:?})")]
    AlreadyTerminal(String, String),
    #[error("task {0:?} must be completed, failed, or cancelled before remove")]
    NotTerminalForRemove(String),
    #[error(
        "task {0:?} cannot be marked completed without a leftover-scope declaration. \
         Call update_task again with result containing either \
         `remaining owned dirty files=<none>` (nothing left) or \
         `remaining owned dirty files=<paths>; residual scope=<why work remains>` \
         (more to do). See the Completion Reporting Contract in this workspace's instructions."
    )]
    MissingCompletionEvidence(String),
    #[error(
        "task {0:?} requires explicit self-verification before being marked completed. \
         Re-read the Completion Reporting Contract checklist: \
         (1) every file you said you'd change is saved and committed where it belongs; \
         (2) tests / build pass if applicable, or you've called out why they don't; \
         (3) `result` already contains the `remaining owned dirty files=` marker in the correct shape; \
         (4) no TODO, skipped branch, or unresolved blocker remains inside this task's scope. \
         Once you've walked through the checklist, call update_task again with `confirm=true`. \
         Do not set `confirm=true` by reflex — the point of this gate is that a human reading \
         your transcript can see you paused to self-verify."
    )]
    CompletionRequiresConfirmation(String),
    #[error("read {path:?}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decode {path:?}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("encode tasks: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("persist tasks: {0}")]
    Persist(String),
}

/// Shared, thread-safe task store. Writers take an exclusive lock;
/// readers return defensive copies so external callers can never
/// mutate internal state.
#[derive(Debug)]
pub struct TaskStore {
    file_path: Option<PathBuf>,
    inner: Mutex<BTreeMap<String, Task>>,
}

#[derive(Debug, Clone)]
pub struct CreateTaskInput {
    pub title: String,
    pub description: String,
    pub assignee: String,
    pub created_by: String,
    pub parent_task_id: String,
    pub start_mode: TaskStartMode,
    pub workflow_mode: TaskWorkflowMode,
    pub priority: TaskPriority,
    pub stale_after_seconds: i64,
    pub dispatch_body: String,
    pub dispatch_config_path: String,
}

impl TaskStore {
    #[must_use]
    pub fn in_memory() -> Arc<Self> {
        Arc::new(Self {
            file_path: None,
            inner: Mutex::new(BTreeMap::new()),
        })
    }

    /// Load tasks from `state_dir`. Checks `tasks-state.json` first
    /// and falls back to the legacy `tasks.json` so upgraded daemons
    /// see their existing task state.
    pub fn load(state_dir: &Path) -> Result<Arc<Self>, TaskStoreError> {
        let primary = state_dir.join(STATE_FILE);
        let legacy = state_dir.join(SNAPSHOT_FILE);
        let path = if primary.exists() {
            primary.clone()
        } else if legacy.exists() {
            legacy
        } else {
            return Ok(Arc::new(Self {
                file_path: Some(primary),
                inner: Mutex::new(BTreeMap::new()),
            }));
        };

        let bytes = std::fs::read(&path).map_err(|source| TaskStoreError::Read {
            path: path.clone(),
            source,
        })?;
        let map = if bytes.is_empty() {
            BTreeMap::new()
        } else {
            let tasks: Vec<Task> =
                serde_json::from_slice(&bytes).map_err(|source| TaskStoreError::Decode {
                    path: path.clone(),
                    source,
                })?;
            tasks
                .into_iter()
                .map(|mut t| {
                    clear_derived_fields(&mut t);
                    (t.id.clone(), t)
                })
                .collect()
        };

        Ok(Arc::new(Self {
            file_path: Some(primary),
            inner: Mutex::new(map),
        }))
    }

    /// Reset tasks the daemon left in `InProgress` whose assignee has
    /// no live session.
    ///
    /// Between a daemon restart and the next successful dispatch, an
    /// `InProgress` task with a dead tmux session is a lie: the agent
    /// isn't actually working on it and no completion signal will ever
    /// arrive. Without this sweep the task sits forever, its rollup
    /// drags the parent open, and the stale detector keeps re-firing
    /// wake attempts at a session that will never answer. Fleet-shell
    /// solves the same problem with `recoverStaleTasks()`; this is the
    /// daemon-side equivalent.
    ///
    /// Only touches `InProgress` tasks — `Blocked` stays alone because
    /// it often encodes an explicit human-held state.
    ///
    /// Returns the IDs that were reset so the caller can log or
    /// telemeter them.
    pub fn recover_stale_in_progress<F>(&self, is_session_live: F) -> Vec<String>
    where
        F: Fn(&str) -> bool,
    {
        let mut inner = self.inner.lock().expect("task store poisoned");
        let now = Utc::now();

        let candidate_ids: Vec<String> = inner
            .values()
            .filter(|t| {
                t.removed_at.is_none()
                    && matches!(t.status, TaskStatus::InProgress)
                    && !t.assignee.is_empty()
                    && !is_session_live(&t.assignee)
            })
            .map(|t| t.id.clone())
            .collect();

        if candidate_ids.is_empty() {
            return Vec::new();
        }

        let mut parent_ids: Vec<String> = Vec::new();
        for id in &candidate_ids {
            let Some(task) = inner.get_mut(id) else {
                continue;
            };
            let assignee = task.assignee.clone();
            task.status = TaskStatus::Pending;
            clear_task_claim(task);
            let _ = clear_task_retry_state(task);
            task.logs.push(TaskLog {
                timestamp: now,
                workspace: OPERATOR_WORKSPACE_NAME.to_owned(),
                message: format!(
                    "Recovery on startup: assignee session {assignee:?} not found; \
                     reset in_progress -> pending"
                ),
            });
            task.version += 1;
            task.updated_at = now;
            if !task.parent_task_id.is_empty() {
                parent_ids.push(task.parent_task_id.clone());
            }
        }

        parent_ids.sort();
        parent_ids.dedup();
        for parent_id in &parent_ids {
            refresh_parent_rollup(&mut inner, parent_id, now);
        }

        // Best-effort persist: a failing write here leaves the store
        // correctly reset in memory and will persist on the next
        // mutation. We surface nothing because startup shouldn't fail
        // on a recovery-only write — real faults will show up on the
        // next normal operation.
        let _ = self.persist_locked(&inner);
        candidate_ids
    }

    pub fn create(&self, input: CreateTaskInput) -> Result<Task, TaskStoreError> {
        let mut inner = self.inner.lock().expect("task store poisoned");

        let now = Utc::now();
        let mut task = Task {
            id: Uuid::new_v4().to_string(),
            title: input.title,
            description: input.description,
            assignee: input.assignee,
            created_by: input.created_by,
            parent_task_id: input.parent_task_id.trim().to_owned(),
            child_task_ids: Vec::new(),
            version: 1,
            status: TaskStatus::Pending,
            start_mode: input.start_mode,
            workflow_mode: Some(input.workflow_mode),
            priority: Some(input.priority),
            stale_after_seconds: input.stale_after_seconds,
            dispatch_message: String::new(),
            dispatch_config_path: input.dispatch_config_path.trim().to_owned(),
            dispatch_count: 0,
            attempt_count: 0,
            last_dispatch_at: None,
            last_attempt_at: None,
            next_retry_at: None,
            claimed_at: None,
            claimed_by: String::new(),
            claim_source: String::new(),
            result: String::new(),
            logs: Vec::new(),
            rollup: None,
            sequence: None,
            stale_info: None,
            removed_at: None,
            removed_by: String::new(),
            remove_reason: String::new(),
            created_at: now,
            updated_at: now,
        };
        let trimmed_body = input.dispatch_body.trim();
        if !trimmed_body.is_empty() {
            task.dispatch_message = format_task_dispatch_message(&task.id, trimmed_body);
        }

        let task_id = task.id.clone();
        let parent_id = task.parent_task_id.clone();
        inner.insert(task_id.clone(), task);

        if !parent_id.is_empty() {
            let Some(parent) = inner.get_mut(&parent_id) else {
                inner.remove(&task_id);
                return Err(TaskStoreError::ParentNotFound(parent_id));
            };
            if parent.removed_at.is_some() {
                inner.remove(&task_id);
                return Err(TaskStoreError::ParentRemoved(parent_id));
            }
            if !parent.child_task_ids.iter().any(|id| id == &task_id) {
                parent.child_task_ids.push(task_id.clone());
                parent.version += 1;
                parent.updated_at = now;
            }
            refresh_parent_rollup(&mut inner, &parent_id, now);
        }

        self.persist_locked(&inner)?;
        Ok(inner
            .get(&task_id)
            .cloned()
            .expect("task just inserted must exist"))
    }

    /// Look up `id`, validate that `workspace` may operate on it, and
    /// confirm the task is still in a state where intervention is
    /// meaningful (pending / `in_progress` / blocked). Mirrors the
    /// guard `handleInterveneTaskEnvelope` runs before dispatching
    /// on the `action` string.
    pub fn get_for_intervention(
        &self,
        id: &str,
        workspace: &str,
        expected_version: Option<i64>,
    ) -> Result<Task, TaskStoreError> {
        let inner = self.inner.lock().expect("task store poisoned");
        let task = inner
            .get(id)
            .ok_or_else(|| TaskStoreError::NotFound(id.to_owned()))?
            .clone();
        validate_task_control(&task, workspace, expected_version, true)?;
        if !matches!(
            task.status,
            TaskStatus::Pending | TaskStatus::InProgress | TaskStatus::Blocked
        ) {
            return Err(TaskStoreError::NotRetryable(id.to_owned()));
        }
        Ok(task)
    }

    pub fn get(&self, id: &str) -> Option<Task> {
        self.inner
            .lock()
            .expect("task store poisoned")
            .get(id)
            .cloned()
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn update(
        &self,
        id: &str,
        status: Option<TaskStatus>,
        result: Option<String>,
        log_msg: Option<String>,
        workspace: &str,
    ) -> Result<Task, TaskStoreError> {
        self.update_with_confirm(id, status, result, log_msg, None, workspace)
    }

    /// Full-shape update, mirroring the wire `UpdateTaskPayload`:
    /// `confirm` is required to transition a task to `Completed`.
    /// Kept as a separate method so unit-test call sites that don't
    /// care about the confirm gate (pre-completion transitions, log
    /// appends, failure paths) stay succinct.
    #[allow(clippy::needless_pass_by_value)]
    pub fn update_with_confirm(
        &self,
        id: &str,
        status: Option<TaskStatus>,
        result: Option<String>,
        log_msg: Option<String>,
        confirm: Option<bool>,
        workspace: &str,
    ) -> Result<Task, TaskStoreError> {
        let mut inner = self.inner.lock().expect("task store poisoned");

        let task = inner
            .get_mut(id)
            .ok_or_else(|| TaskStoreError::NotFound(id.to_owned()))?;
        validate_task_update(
            task,
            status.as_ref(),
            result.as_deref(),
            log_msg.as_deref(),
            confirm,
            workspace,
        )?;

        let now = Utc::now();
        let mut changed = false;
        let prev_status = task.status.clone();

        let action_source = claim_source(status.as_ref(), result.as_deref(), log_msg.as_deref());
        if workspace == task.assignee
            && action_source.is_some()
            && mark_task_claimed(task, workspace, action_source.as_deref().unwrap_or(""), now)
        {
            changed = true;
        }
        if let Some(new_status) = status.as_ref() {
            if &task.status != new_status {
                task.status = new_status.clone();
                changed = true;
            }
            if (matches!(new_status, TaskStatus::Blocked) || is_terminal_status(new_status))
                && clear_task_retry_state(task)
            {
                changed = true;
            }
        }
        if let Some(new_result) = result.as_deref() {
            if task.result != new_result {
                new_result.clone_into(&mut task.result);
                changed = true;
            }
        }
        if let Some(message) = log_msg.as_deref() {
            if !is_duplicate_task_log(task, workspace, message, now) {
                task.logs.push(TaskLog {
                    timestamp: now,
                    workspace: workspace.to_owned(),
                    message: message.to_owned(),
                });
                changed = true;
            }
        }

        // Soft evidence hint. Fires only on the edge into `Completed`,
        // and only when the effective result has no file paths or
        // recognisable tool-command fragments. Left as a task log —
        // NOT as a rejected update — because "evidence" is fuzzy and
        // legitimate completions can read terse. Humans (and the TUI)
        // see the note and know to spot-check.
        if matches!(task.status, TaskStatus::Completed)
            && !matches!(prev_status, TaskStatus::Completed)
            && !has_concrete_evidence(&task.result)
        {
            task.logs.push(TaskLog {
                timestamp: now,
                workspace: OPERATOR_WORKSPACE_NAME.to_owned(),
                message:
                    "evidence hint: completion result contains no file paths or tool-command \
                     fragments. Reviewers may want to spot-check — the Completion Reporting \
                     Contract suggests mentioning the files you touched or the command(s) \
                     that verified the change."
                        .to_owned(),
            });
            changed = true;
        }

        let parent_id = task.parent_task_id.clone();
        if changed {
            task.version += 1;
            task.updated_at = now;
            refresh_parent_rollup(&mut inner, &parent_id, now);
            self.persist_locked(&inner)?;
        }
        Ok(inner.get(id).cloned().expect("task existed above"))
    }

    pub fn cancel(
        &self,
        id: &str,
        reason: &str,
        workspace: &str,
        expected_version: Option<i64>,
    ) -> Result<Task, TaskStoreError> {
        let mut inner = self.inner.lock().expect("task store poisoned");

        let task = inner
            .get_mut(id)
            .ok_or_else(|| TaskStoreError::NotFound(id.to_owned()))?;
        validate_task_control(task, workspace, expected_version, true)?;
        if is_terminal_status(&task.status) {
            return Err(TaskStoreError::AlreadyTerminal(
                id.to_owned(),
                format!("{:?}", task.status).to_ascii_lowercase(),
            ));
        }

        let now = Utc::now();
        let trimmed = reason.trim();
        let msg = if trimmed.is_empty() {
            format!("Cancelled by {workspace}")
        } else {
            format!("Cancelled by {workspace}: {trimmed}")
        };
        if workspace == task.assignee {
            let _ = mark_task_claimed(task, workspace, "cancel", now);
        }
        task.status = TaskStatus::Cancelled;
        task.result.clone_from(&msg);
        task.logs.push(TaskLog {
            timestamp: now,
            workspace: workspace.to_owned(),
            message: msg,
        });
        task.version += 1;
        task.updated_at = now;
        let parent_id = task.parent_task_id.clone();
        refresh_parent_rollup(&mut inner, &parent_id, now);
        self.persist_locked(&inner)?;
        Ok(inner.get(id).cloned().expect("task existed above"))
    }

    pub fn remove(
        &self,
        id: &str,
        reason: &str,
        workspace: &str,
        expected_version: Option<i64>,
    ) -> Result<Task, TaskStoreError> {
        let mut inner = self.inner.lock().expect("task store poisoned");

        let task = inner
            .get_mut(id)
            .ok_or_else(|| TaskStoreError::NotFound(id.to_owned()))?;
        validate_task_control(task, workspace, expected_version, false)?;
        if task.removed_at.is_some() {
            return Ok(task.clone());
        }
        if !is_terminal_status(&task.status) {
            return Err(TaskStoreError::NotTerminalForRemove(id.to_owned()));
        }

        let now = Utc::now();
        task.removed_at = Some(now);
        workspace.clone_into(&mut task.removed_by);
        reason.trim().clone_into(&mut task.remove_reason);
        task.version += 1;
        let parent_id = task.parent_task_id.clone();
        refresh_parent_rollup(&mut inner, &parent_id, now);
        self.persist_locked(&inner)?;
        Ok(inner.get(id).cloned().expect("task existed above"))
    }

    pub fn retry(
        &self,
        id: &str,
        note: &str,
        workspace: &str,
        expected_version: Option<i64>,
    ) -> Result<Task, TaskStoreError> {
        let mut inner = self.inner.lock().expect("task store poisoned");

        let task = inner
            .get_mut(id)
            .ok_or_else(|| TaskStoreError::NotFound(id.to_owned()))?;
        validate_task_control(task, workspace, expected_version, true)?;
        if !matches!(
            task.status,
            TaskStatus::Pending | TaskStatus::InProgress | TaskStatus::Blocked
        ) {
            return Err(TaskStoreError::NotRetryable(id.to_owned()));
        }

        let now = Utc::now();
        task.status = TaskStatus::Pending;
        task.result.clear();
        clear_task_claim(task);
        let _ = clear_task_retry_state(task);
        let trimmed = note.trim();
        let msg = if trimmed.is_empty() {
            format!("Recovery action: retry requested by {workspace}")
        } else {
            format!("Recovery action: retry requested by {workspace}: {trimmed}")
        };
        task.logs.push(TaskLog {
            timestamp: now,
            workspace: workspace.to_owned(),
            message: msg,
        });
        task.version += 1;
        task.updated_at = now;
        let parent_id = task.parent_task_id.clone();
        refresh_parent_rollup(&mut inner, &parent_id, now);
        self.persist_locked(&inner)?;
        Ok(inner.get(id).cloned().expect("task existed above"))
    }

    pub fn list(&self, assignee: &str, created_by: &str, status: Option<&TaskStatus>) -> Vec<Task> {
        let inner = self.inner.lock().expect("task store poisoned");
        inner
            .values()
            .filter(|task| {
                if task.removed_at.is_some() {
                    return false;
                }
                if !assignee.is_empty() && task.assignee != assignee {
                    return false;
                }
                if !created_by.is_empty() && task.created_by != created_by {
                    return false;
                }
                if let Some(want) = status {
                    if &task.status != want {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    }

    /// Heartbeat the task by bumping `updated_at` when the assignee
    /// records MCP tool activity tagged with this task. Only mutates
    /// live (`InProgress`) tasks assigned to `by`; everything else
    /// is a silent no-op so malformed or speculative task_ids cannot
    /// reshape state. Returns the snapshot on success.
    pub fn mark_tool_activity(
        &self,
        id: &str,
        by: &str,
        when: DateTime<Utc>,
    ) -> Option<Task> {
        let mut inner = self.inner.lock().expect("task store poisoned");
        let task = inner.get_mut(id)?;
        if task.removed_at.is_some()
            || !matches!(task.status, TaskStatus::InProgress)
            || task.assignee != by
        {
            return None;
        }
        let stamp = if when.timestamp() == 0 {
            Utc::now()
        } else {
            when
        };
        task.updated_at = stamp;
        task.version += 1;
        let snapshot = task.clone();
        let _ = self.persist_locked(&inner);
        Some(snapshot)
    }

    pub fn record_dispatch(&self, id: &str, to: &str, when: DateTime<Utc>) -> Option<Task> {
        let mut inner = self.inner.lock().expect("task store poisoned");
        let task = inner.get_mut(id)?;
        if task.removed_at.is_some() || task.assignee != to {
            return None;
        }
        task.dispatch_count += 1;
        let stamp = if when.timestamp() == 0 {
            Utc::now()
        } else {
            when
        };
        task.last_dispatch_at = Some(stamp);
        task.updated_at = stamp;
        task.version += 1;
        let snapshot = task.clone();
        // Persist asynchronously to disk; failure here is logged by
        // the caller (record_dispatch is best-effort).
        let _ = self.persist_locked(&inner);
        Some(snapshot)
    }

    /// List `InProgress` tasks whose assignee is set and whose most
    /// recent update is older than `stale_threshold`. Caller is
    /// responsible for liveness/idle/inbox checks and for deciding
    /// whether to nudge — this is a read-only selector so it can be
    /// called from background loops without locking against writer
    /// paths for long.
    ///
    /// Compared to `recover_stale_in_progress`, which resets tasks
    /// whose assignee is *gone*, this selector targets the subtler
    /// "assignee is alive but forgot to close the loop" case.
    pub fn list_silent_in_progress(
        &self,
        now: DateTime<Utc>,
        stale_threshold: chrono::Duration,
    ) -> Vec<Task> {
        let inner = self.inner.lock().expect("task store poisoned");
        inner
            .values()
            .filter(|task| {
                if task.removed_at.is_some()
                    || !matches!(task.status, TaskStatus::InProgress)
                    || task.assignee.is_empty()
                {
                    return false;
                }
                now.signed_duration_since(task.updated_at) >= stale_threshold
            })
            .cloned()
            .collect()
    }

    pub fn runnable_by_assignee(&self, assignee: &str, now: DateTime<Utc>) -> Vec<Task> {
        let inner = self.inner.lock().expect("task store poisoned");
        inner
            .values()
            .filter(|task| {
                if task.removed_at.is_some() || task.assignee != assignee {
                    return false;
                }
                if !matches!(task.status, TaskStatus::Pending)
                    || task.last_dispatch_at.is_none()
                    || task.claimed_at.is_some()
                {
                    return false;
                }
                if let Some(retry) = task.next_retry_at {
                    if retry > now {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    }

    pub fn snapshot(&self) -> Vec<Task> {
        self.inner
            .lock()
            .expect("task store poisoned")
            .values()
            .cloned()
            .collect()
    }

    fn persist_locked(&self, inner: &BTreeMap<String, Task>) -> Result<(), TaskStoreError> {
        let Some(path) = &self.file_path else {
            return Ok(());
        };
        let mut tasks: Vec<Task> = inner
            .values()
            .cloned()
            .map(|mut t| {
                clear_derived_fields(&mut t);
                t
            })
            .collect();
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        let bytes = serde_json::to_vec(&tasks)?;
        write_file_atomic(path, &bytes).map_err(|e| TaskStoreError::Persist(e.to_string()))
    }
}

// ---------- validation ----------

fn validate_task_update(
    task: &Task,
    status: Option<&TaskStatus>,
    result: Option<&str>,
    _log_msg: Option<&str>,
    confirm: Option<bool>,
    workspace: &str,
) -> Result<(), TaskStoreError> {
    if task.removed_at.is_some() {
        return Err(TaskStoreError::Removed(task.id.clone()));
    }
    if workspace != task.assignee && workspace != task.created_by {
        return Err(TaskStoreError::UnauthorisedUpdate(
            workspace.to_owned(),
            task.id.clone(),
        ));
    }
    if let Some(r) = result {
        if !r.trim().is_empty() && workspace != task.assignee {
            return Err(TaskStoreError::UnauthorisedResult(
                workspace.to_owned(),
                task.id.clone(),
                task.assignee.clone(),
            ));
        }
    }
    let Some(new_status) = status else {
        return Ok(());
    };
    if workspace != task.assignee && new_status != &task.status {
        return Err(TaskStoreError::UnauthorisedStatus(
            workspace.to_owned(),
            task.id.clone(),
            task.assignee.clone(),
        ));
    }
    if !is_allowed_transition(&task.status, new_status) {
        return Err(TaskStoreError::InvalidTransition(
            status_label(&task.status).to_owned(),
            status_label(new_status).to_owned(),
        ));
    }
    if matches!(new_status, TaskStatus::Completed)
        && !matches!(task.status, TaskStatus::Completed)
    {
        let effective = result.unwrap_or(task.result.as_str());
        if !has_leftover_scope_declaration(effective) {
            return Err(TaskStoreError::MissingCompletionEvidence(task.id.clone()));
        }
        if !confirm.unwrap_or(false) {
            return Err(TaskStoreError::CompletionRequiresConfirmation(
                task.id.clone(),
            ));
        }
    }
    Ok(())
}

/// The Completion Reporting Contract (see
/// `crates/ax-workspace/src/instructions.rs::completion_reporting_instruction_contract`)
/// requires every completion result to declare what owned files, if
/// any, are still dirty. Daemon enforcement keeps that contract
/// load-bearing instead of advisory: an assignee that marks
/// `status=completed` without the marker gets rejected with a clear
/// remediation path.
fn has_leftover_scope_declaration(result: &str) -> bool {
    const MARKER: &str = "remaining owned dirty files=";
    result.contains(MARKER)
}

fn validate_task_control(
    task: &Task,
    workspace: &str,
    expected_version: Option<i64>,
    allow_assignee: bool,
) -> Result<(), TaskStoreError> {
    if task.removed_at.is_some() {
        return Err(TaskStoreError::Removed(task.id.clone()));
    }
    if let Some(want) = expected_version {
        if task.version != want {
            return Err(TaskStoreError::VersionMismatch(
                task.id.clone(),
                task.version,
                want,
            ));
        }
    }
    if workspace == OPERATOR_WORKSPACE_NAME || workspace == task.created_by {
        return Ok(());
    }
    if allow_assignee && workspace == task.assignee {
        return Ok(());
    }
    Err(TaskStoreError::UnauthorisedControl(
        workspace.to_owned(),
        task.id.clone(),
    ))
}

fn is_allowed_transition(current: &TaskStatus, next: &TaskStatus) -> bool {
    if current == next {
        return true;
    }
    match current {
        TaskStatus::Pending => matches!(
            next,
            TaskStatus::InProgress
                | TaskStatus::Blocked
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
        ),
        TaskStatus::InProgress => matches!(
            next,
            TaskStatus::Blocked
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
        ),
        TaskStatus::Blocked => matches!(
            next,
            TaskStatus::Pending
                | TaskStatus::InProgress
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
        ),
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled => false,
    }
}

pub(crate) fn is_terminal_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

fn status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn claim_source(
    status: Option<&TaskStatus>,
    result: Option<&str>,
    log_msg: Option<&str>,
) -> Option<String> {
    if let Some(s) = status {
        return Some(format!("status:{}", status_label(s)));
    }
    if result.is_some() {
        return Some("result".to_owned());
    }
    if log_msg.is_some() {
        return Some("log".to_owned());
    }
    None
}

fn mark_task_claimed(task: &mut Task, workspace: &str, source: &str, now: DateTime<Utc>) -> bool {
    if task.claimed_at.is_some() || workspace != task.assignee {
        return false;
    }
    task.claimed_at = Some(now);
    workspace.clone_into(&mut task.claimed_by);
    source.clone_into(&mut task.claim_source);
    task.attempt_count += 1;
    task.last_attempt_at = Some(now);
    task.next_retry_at = None;
    true
}

fn clear_task_claim(task: &mut Task) {
    task.claimed_at = None;
    task.claimed_by.clear();
    task.claim_source.clear();
}

fn clear_task_retry_state(task: &mut Task) -> bool {
    if task.next_retry_at.is_none() {
        return false;
    }
    task.next_retry_at = None;
    true
}

fn is_duplicate_task_log(task: &Task, workspace: &str, log_msg: &str, now: DateTime<Utc>) -> bool {
    let normalized = normalize_message_for_suppression(log_msg);
    if normalized.is_empty() || !looks_like_noop_status_message(&normalized) || task.logs.is_empty()
    {
        return false;
    }
    let last = task.logs.last().expect("non-empty checked above");
    if last.workspace != workspace {
        return false;
    }
    let elapsed = now.signed_duration_since(last.timestamp);
    if elapsed
        .to_std()
        .map_or(true, |d| d > DUPLICATE_SUPPRESSION_WINDOW)
    {
        return false;
    }
    normalize_message_for_suppression(&last.message) == normalized
}

fn clear_derived_fields(task: &mut Task) {
    task.sequence = None;
    task.stale_info = None;
}

// ---------- rollup ----------

fn refresh_parent_rollup(inner: &mut BTreeMap<String, Task>, parent_id: &str, now: DateTime<Utc>) {
    let parent_id = parent_id.trim();
    if parent_id.is_empty() {
        return;
    }
    let Some(parent) = inner.get(parent_id) else {
        return;
    };
    if parent.removed_at.is_some() {
        return;
    }
    let rollup = summarize_task_rollup(parent, inner);
    let Some(parent) = inner.get_mut(parent_id) else {
        return;
    };
    let current = parent.rollup.clone();
    let mut changed = !rollup_equal(current.as_ref(), rollup.as_ref());
    parent.rollup = rollup;
    if parent.rollup.is_none() {
        if changed {
            parent.version += 1;
            parent.updated_at = now;
        }
        return;
    }
    let summary = parent
        .rollup
        .as_ref()
        .map(|r| r.summary.clone())
        .unwrap_or_default();
    if !summary.is_empty() && should_append_rollup_log(parent, &summary) {
        parent.logs.push(TaskLog {
            timestamp: now,
            workspace: parent.assignee.clone(),
            message: summary,
        });
        changed = true;
    }
    if changed {
        parent.version += 1;
        parent.updated_at = now;
    }
}

fn summarize_task_rollup(parent: &Task, all: &BTreeMap<String, Task>) -> Option<TaskRollup> {
    if parent.child_task_ids.is_empty() {
        return None;
    }
    let mut rollup = TaskRollup {
        total_children: parent.child_task_ids.len() as i64,
        pending_children: 0,
        in_progress_children: 0,
        blocked_children: 0,
        completed_children: 0,
        failed_children: 0,
        cancelled_children: 0,
        terminal_children: 0,
        active_children: 0,
        last_child_update_at: None,
        all_children_terminal: false,
        needs_parent_reconciliation: false,
        summary: String::new(),
    };
    for child_id in &parent.child_task_ids {
        let Some(child) = all.get(child_id) else {
            continue;
        };
        match child.status {
            TaskStatus::Pending => rollup.pending_children += 1,
            TaskStatus::InProgress => rollup.in_progress_children += 1,
            TaskStatus::Blocked => rollup.blocked_children += 1,
            TaskStatus::Completed => {
                rollup.completed_children += 1;
                rollup.terminal_children += 1;
            }
            TaskStatus::Failed => {
                rollup.failed_children += 1;
                rollup.terminal_children += 1;
            }
            TaskStatus::Cancelled => {
                rollup.cancelled_children += 1;
                rollup.terminal_children += 1;
            }
        }
        if matches!(child.status, TaskStatus::Pending | TaskStatus::InProgress) {
            rollup.active_children += 1;
        }
        match rollup.last_child_update_at {
            None => rollup.last_child_update_at = Some(child.updated_at),
            Some(ts) if child.updated_at > ts => {
                rollup.last_child_update_at = Some(child.updated_at);
            }
            _ => {}
        }
    }
    rollup.all_children_terminal =
        rollup.total_children > 0 && rollup.terminal_children == rollup.total_children;
    rollup.needs_parent_reconciliation =
        rollup.all_children_terminal && !is_terminal_status(&parent.status);
    rollup.summary = format_rollup_summary(parent, &rollup);
    Some(rollup)
}

fn format_rollup_summary(parent: &Task, rollup: &TaskRollup) -> String {
    let mut base = format!(
        "Child rollup: total={} active={} completed={} failed={} cancelled={} pending={} in_progress={} blocked={}.",
        rollup.total_children,
        rollup.active_children,
        rollup.completed_children,
        rollup.failed_children,
        rollup.cancelled_children,
        rollup.pending_children,
        rollup.in_progress_children,
        rollup.blocked_children,
    );
    if rollup.needs_parent_reconciliation {
        base.push_str(" All child tasks are terminal; parent reconciliation is still required.");
        return base;
    }
    if rollup.all_children_terminal {
        base.push_str(" All child tasks are terminal.");
        return base;
    }
    if matches!(parent.status, TaskStatus::Pending) {
        base.push_str(" Parent is waiting on child progress.");
        return base;
    }
    base.push_str(" Parent remains open while child work is still active.");
    base
}

fn should_append_rollup_log(task: &Task, msg: &str) -> bool {
    if msg.is_empty() {
        return false;
    }
    match task.logs.last() {
        None => true,
        Some(last) => last.message != msg,
    }
}

fn rollup_equal(a: Option<&TaskRollup>, b: Option<&TaskRollup>) -> bool {
    match (a, b) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(x), Some(y)) => {
            x.total_children == y.total_children
                && x.pending_children == y.pending_children
                && x.in_progress_children == y.in_progress_children
                && x.blocked_children == y.blocked_children
                && x.completed_children == y.completed_children
                && x.failed_children == y.failed_children
                && x.cancelled_children == y.cancelled_children
                && x.terminal_children == y.terminal_children
                && x.active_children == y.active_children
                && x.all_children_terminal == y.all_children_terminal
                && x.needs_parent_reconciliation == y.needs_parent_reconciliation
                && x.summary == y.summary
                && x.last_child_update_at == y.last_child_update_at
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> Arc<TaskStore> {
        TaskStore::in_memory()
    }

    fn seed_in_progress_task(store: &TaskStore) -> Task {
        let task = store
            .create(CreateTaskInput {
                title: "t".into(),
                description: String::new(),
                assignee: "worker".into(),
                created_by: "orch".into(),
                parent_task_id: String::new(),
                start_mode: TaskStartMode::Default,
                workflow_mode: TaskWorkflowMode::Parallel,
                priority: TaskPriority::Normal,
                stale_after_seconds: 0,
                dispatch_body: String::new(),
                dispatch_config_path: String::new(),
            })
            .expect("create");
        store
            .update(
                &task.id,
                Some(TaskStatus::InProgress),
                None,
                None,
                "worker",
            )
            .expect("to in_progress");
        store.get(&task.id).expect("task")
    }

    #[test]
    fn completion_without_marker_is_rejected() {
        let store = make_store();
        let task = seed_in_progress_task(&store);

        let err = store
            .update(
                &task.id,
                Some(TaskStatus::Completed),
                Some("done".into()),
                None,
                "worker",
            )
            .expect_err("should reject");
        assert!(
            matches!(err, TaskStoreError::MissingCompletionEvidence(ref id) if id == &task.id),
            "expected MissingCompletionEvidence, got {err:?}"
        );
    }

    #[test]
    fn completion_with_none_marker_is_accepted() {
        let store = make_store();
        let task = seed_in_progress_task(&store);

        let updated = store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                Some("wired the validator; remaining owned dirty files=<none>".into()),
                None,
                Some(true),
                "worker",
            )
            .expect("should accept");
        assert!(matches!(updated.status, TaskStatus::Completed));
    }

    #[test]
    fn completion_with_paths_and_residual_scope_is_accepted() {
        let store = make_store();
        let task = seed_in_progress_task(&store);

        let body = "partial pass; remaining owned dirty files=src/foo.rs; residual scope=finish foo refactor in follow-up";
        let updated = store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                Some(body.into()),
                None,
                Some(true),
                "worker",
            )
            .expect("should accept");
        assert!(matches!(updated.status, TaskStatus::Completed));
    }

    #[test]
    fn completion_marker_can_live_in_prior_result() {
        // Agents can split the workflow: set result with the marker
        // first, then flip status to completed without passing result
        // again. Enforcing only on the effective result keeps that
        // path open instead of forcing every completion into a single
        // call.
        let store = make_store();
        let task = seed_in_progress_task(&store);
        store
            .update(
                &task.id,
                None,
                Some("prep; remaining owned dirty files=<none>".into()),
                None,
                "worker",
            )
            .expect("set result");

        let updated = store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                None,
                None,
                Some(true),
                "worker",
            )
            .expect("should accept");
        assert!(matches!(updated.status, TaskStatus::Completed));
    }

    #[test]
    fn completion_without_confirm_is_rejected_after_marker_passes() {
        // Marker present but confirm missing: the daemon must still
        // reject, and the error names the checklist gate so the
        // caller knows what to do next.
        let store = make_store();
        let task = seed_in_progress_task(&store);

        let err = store
            .update(
                &task.id,
                Some(TaskStatus::Completed),
                Some("done; remaining owned dirty files=<none>".into()),
                None,
                "worker",
            )
            .expect_err("confirm gate must trip");
        assert!(
            matches!(
                err,
                TaskStoreError::CompletionRequiresConfirmation(ref id) if id == &task.id
            ),
            "expected CompletionRequiresConfirmation, got {err:?}"
        );
    }

    #[test]
    fn completion_gate_order_marker_runs_before_confirm() {
        // When both preconditions fail, the caller sees the marker
        // complaint first. Ordering matters: fixing the result shape
        // is a structural prerequisite for the confirm step having
        // anything meaningful to affirm.
        let store = make_store();
        let task = seed_in_progress_task(&store);

        let err = store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                Some("done".into()), // no marker
                None,
                None, // no confirm
                "worker",
            )
            .expect_err("both preconditions missing");
        assert!(
            matches!(err, TaskStoreError::MissingCompletionEvidence(_)),
            "marker complaint must surface first, got {err:?}"
        );
    }

    #[test]
    fn completion_without_evidence_signal_adds_hint_log() {
        // Happy path: marker present, confirm true, but the result
        // reads as a handwave. The store accepts (the hard gates
        // passed) but records a log line so reviewers see the
        // evidence was light.
        let store = make_store();
        let task = seed_in_progress_task(&store);

        let updated = store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                Some("done; remaining owned dirty files=<none>".into()),
                None,
                Some(true),
                "worker",
            )
            .expect("should accept with hint log");
        assert!(matches!(updated.status, TaskStatus::Completed));
        assert!(
            updated
                .logs
                .iter()
                .any(|l| l.message.contains("evidence hint")),
            "expected evidence-hint log, got {:?}",
            updated.logs
        );
    }

    #[test]
    fn completion_with_evidence_signal_skips_hint_log() {
        let store = make_store();
        let task = seed_in_progress_task(&store);

        let updated = store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                Some(
                    "wrote src/foo.rs and ran cargo test; remaining owned dirty files=<none>"
                        .into(),
                ),
                None,
                Some(true),
                "worker",
            )
            .expect("should accept without hint");
        assert!(
            !updated
                .logs
                .iter()
                .any(|l| l.message.contains("evidence hint")),
            "concrete evidence must suppress the hint, got {:?}",
            updated.logs
        );
    }

    #[test]
    fn completion_with_confirm_false_still_rejected() {
        let store = make_store();
        let task = seed_in_progress_task(&store);

        let err = store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                Some("done; remaining owned dirty files=<none>".into()),
                None,
                Some(false),
                "worker",
            )
            .expect_err("explicit confirm=false must still fail");
        assert!(
            matches!(err, TaskStoreError::CompletionRequiresConfirmation(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn failed_status_does_not_require_marker() {
        let store = make_store();
        let task = seed_in_progress_task(&store);

        let updated = store
            .update(
                &task.id,
                Some(TaskStatus::Failed),
                Some("blocked by missing dependency".into()),
                None,
                "worker",
            )
            .expect("failed without marker is fine");
        assert!(matches!(updated.status, TaskStatus::Failed));
    }

    #[test]
    fn already_completed_noop_update_skips_marker_check() {
        let store = make_store();
        let task = seed_in_progress_task(&store);
        store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                Some("first pass; remaining owned dirty files=<none>".into()),
                None,
                Some(true),
                "worker",
            )
            .expect("initial complete");

        // Same-status update that only appends a log should not
        // re-trigger the marker requirement. The transition guard is
        // scoped to the Pending/InProgress -> Completed edge.
        let updated = store
            .update(
                &task.id,
                Some(TaskStatus::Completed),
                None,
                Some("post-completion note".into()),
                "worker",
            )
            .expect("noop completed update should succeed");
        assert!(matches!(updated.status, TaskStatus::Completed));
    }

    #[test]
    fn has_leftover_scope_declaration_matches_contract_shapes() {
        assert!(has_leftover_scope_declaration(
            "remaining owned dirty files=<none>"
        ));
        assert!(has_leftover_scope_declaration(
            "result text; remaining owned dirty files=src/a.rs; residual scope=x"
        ));
        assert!(!has_leftover_scope_declaration("just a plain summary"));
        assert!(!has_leftover_scope_declaration(""));
    }

    #[test]
    fn recover_stale_in_progress_resets_only_dead_sessions() {
        let store = make_store();
        let alive = seed_in_progress_task(&store); // assignee "worker"
        let dead = seed_in_progress_task(&store); // assignee "worker"

        // Pending task must never be touched.
        let pending = store
            .create(CreateTaskInput {
                title: "untouched".into(),
                description: String::new(),
                assignee: "worker".into(),
                created_by: "orch".into(),
                parent_task_id: String::new(),
                start_mode: TaskStartMode::Default,
                workflow_mode: TaskWorkflowMode::Parallel,
                priority: TaskPriority::Normal,
                stale_after_seconds: 0,
                dispatch_body: String::new(),
                dispatch_config_path: String::new(),
            })
            .expect("create pending");

        // Simulate "alive" by returning true only for the first task's
        // assignee. seed_in_progress_task uses the same assignee name
        // for both, so this test steers on task id instead — bind the
        // closure to the live id.
        let live_id = alive.id.clone();
        let live_map: std::collections::HashMap<String, bool> = {
            let mut m = std::collections::HashMap::new();
            m.insert(live_id.clone(), true);
            m
        };
        // Because both tasks share the same assignee, the recovery
        // sweep actually treats them as a single liveness question. To
        // test the "one alive, one dead" shape we need distinct
        // assignees. Rewrite `dead`'s assignee in-place for the test.
        {
            let mut inner = store.inner.lock().expect("lock");
            let t = inner.get_mut(&dead.id).expect("dead task");
            "worker-dead".clone_into(&mut t.assignee);
        }
        let _ = live_map;

        let reset = store.recover_stale_in_progress(|ws| ws == "worker");
        assert_eq!(reset, vec![dead.id.clone()]);

        let after_alive = store.get(&alive.id).expect("alive");
        assert!(matches!(after_alive.status, TaskStatus::InProgress));
        let after_dead = store.get(&dead.id).expect("dead");
        assert!(matches!(after_dead.status, TaskStatus::Pending));
        assert!(after_dead.claimed_at.is_none());
        assert!(after_dead
            .logs
            .last()
            .is_some_and(|l| l.message.contains("Recovery on startup")));

        let after_pending = store.get(&pending.id).expect("pending");
        assert!(matches!(after_pending.status, TaskStatus::Pending));
    }

    #[test]
    fn recover_stale_in_progress_skips_terminal_tasks() {
        let store = make_store();
        let task = seed_in_progress_task(&store);
        store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                Some("done; remaining owned dirty files=<none>".into()),
                None,
                Some(true),
                "worker",
            )
            .expect("complete");

        let reset = store.recover_stale_in_progress(|_ws| false);
        assert!(reset.is_empty());
        let reloaded = store.get(&task.id).expect("task");
        assert!(matches!(reloaded.status, TaskStatus::Completed));
    }

    #[test]
    fn list_silent_in_progress_matches_idle_shape() {
        let store = make_store();
        let task = seed_in_progress_task(&store);
        // Freshly-updated task: inside the threshold, should not match.
        let now = Utc::now();
        let fresh = store.list_silent_in_progress(now, chrono::Duration::seconds(60));
        assert!(fresh.is_empty(), "fresh task must not appear silent");

        // Reach in and age the task past the threshold. This mimics
        // the real condition where the agent's last update was
        // minutes ago.
        {
            let mut inner = store.inner.lock().expect("lock");
            let t = inner.get_mut(&task.id).expect("task");
            t.updated_at = now - chrono::Duration::seconds(200);
        }
        let stale = store.list_silent_in_progress(now, chrono::Duration::seconds(60));
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].id, task.id);
    }

    #[test]
    fn list_silent_in_progress_excludes_terminal_and_pending_and_removed() {
        let store = make_store();
        // Pending: never matches.
        let pending = seed_in_progress_task(&store);
        {
            let mut inner = store.inner.lock().expect("lock");
            let t = inner.get_mut(&pending.id).expect("task");
            t.status = TaskStatus::Pending;
            t.updated_at = Utc::now() - chrono::Duration::seconds(500);
        }

        // Completed: never matches.
        let completed = seed_in_progress_task(&store);
        store
            .update_with_confirm(
                &completed.id,
                Some(TaskStatus::Completed),
                Some("done; remaining owned dirty files=<none>".into()),
                None,
                Some(true),
                "worker",
            )
            .expect("complete");

        // Removed: never matches even if in_progress + old.
        let removed = seed_in_progress_task(&store);
        {
            let mut inner = store.inner.lock().expect("lock");
            let t = inner.get_mut(&removed.id).expect("task");
            t.removed_at = Some(Utc::now());
            t.updated_at = Utc::now() - chrono::Duration::seconds(500);
        }

        // Empty-assignee in_progress: never matches (shouldn't happen
        // in normal flow but the filter should hold anyway).
        let orphan = seed_in_progress_task(&store);
        {
            let mut inner = store.inner.lock().expect("lock");
            let t = inner.get_mut(&orphan.id).expect("task");
            t.assignee.clear();
            t.updated_at = Utc::now() - chrono::Duration::seconds(500);
        }

        let silent = store
            .list_silent_in_progress(Utc::now(), chrono::Duration::seconds(60));
        assert!(silent.is_empty(), "selector should reject all fixtures");
    }

    #[test]
    fn mark_tool_activity_refreshes_updated_at_for_live_assignee() {
        let store = make_store();
        let task = seed_in_progress_task(&store);
        // Age the task so it would show up as silent without activity.
        {
            let mut inner = store.inner.lock().expect("lock");
            let t = inner.get_mut(&task.id).expect("task");
            t.updated_at = Utc::now() - chrono::Duration::seconds(500);
        }
        let before = store.get(&task.id).expect("task").updated_at;

        let now = Utc::now();
        let snapshot = store
            .mark_tool_activity(&task.id, "worker", now)
            .expect("live InProgress task assigned to worker must heartbeat");
        assert!(snapshot.updated_at > before);

        let silent = store.list_silent_in_progress(now, chrono::Duration::seconds(60));
        assert!(
            silent.is_empty(),
            "heartbeat must clear silent-exit state"
        );
    }

    #[test]
    fn mark_tool_activity_rejects_wrong_assignee() {
        let store = make_store();
        let task = seed_in_progress_task(&store);
        let result = store.mark_tool_activity(&task.id, "someone-else", Utc::now());
        assert!(result.is_none());
    }

    #[test]
    fn mark_tool_activity_rejects_non_in_progress_task() {
        let store = make_store();
        let task = seed_in_progress_task(&store);
        // Push to a terminal status.
        store
            .update_with_confirm(
                &task.id,
                Some(TaskStatus::Completed),
                Some("done; remaining owned dirty files=<none>".into()),
                None,
                Some(true),
                "worker",
            )
            .expect("complete");
        let result = store.mark_tool_activity(&task.id, "worker", Utc::now());
        assert!(result.is_none());
    }

    #[test]
    fn mark_tool_activity_rejects_unknown_task() {
        let store = make_store();
        let result = store.mark_tool_activity("t-ghost-99", "worker", Utc::now());
        assert!(result.is_none());
    }

    #[test]
    fn recover_stale_in_progress_refreshes_parent_rollup() {
        let store = make_store();
        let parent = store
            .create(CreateTaskInput {
                title: "parent".into(),
                description: String::new(),
                assignee: "orch".into(),
                created_by: "orch".into(),
                parent_task_id: String::new(),
                start_mode: TaskStartMode::Default,
                workflow_mode: TaskWorkflowMode::Parallel,
                priority: TaskPriority::Normal,
                stale_after_seconds: 0,
                dispatch_body: String::new(),
                dispatch_config_path: String::new(),
            })
            .expect("parent");
        let child = store
            .create(CreateTaskInput {
                title: "child".into(),
                description: String::new(),
                assignee: "worker".into(),
                created_by: "orch".into(),
                parent_task_id: parent.id.clone(),
                start_mode: TaskStartMode::Default,
                workflow_mode: TaskWorkflowMode::Parallel,
                priority: TaskPriority::Normal,
                stale_after_seconds: 0,
                dispatch_body: String::new(),
                dispatch_config_path: String::new(),
            })
            .expect("child");
        store
            .update(
                &child.id,
                Some(TaskStatus::InProgress),
                None,
                None,
                "worker",
            )
            .expect("to in_progress");

        let parent_before = store.get(&parent.id).expect("parent");
        let rollup_before = parent_before.rollup.expect("rollup");
        assert_eq!(rollup_before.in_progress_children, 1);
        assert_eq!(rollup_before.pending_children, 0);

        let reset = store.recover_stale_in_progress(|_ws| false);
        assert_eq!(reset, vec![child.id.clone()]);

        let parent_after = store.get(&parent.id).expect("parent");
        let rollup_after = parent_after.rollup.expect("rollup");
        assert_eq!(rollup_after.in_progress_children, 0);
        assert_eq!(rollup_after.pending_children, 1);
    }
}
