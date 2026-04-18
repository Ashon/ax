//! Persistent task store for the daemon. Mirrors
//! `internal/daemon/taskstore.go` field-for-field: the JSON on disk is
//! a sorted `Vec<Task>`, derived fields (`sequence`, `stale_info`) are
//! stripped before persistence, and every mutation refreshes the
//! parent rollup in place.
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
    format_task_dispatch_message, looks_like_noop_status_message,
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
    /// and falls back to the legacy `tasks.json` so daemons upgraded
    /// from the Go build see their existing task state.
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
    /// guard Go's `handleInterveneTaskEnvelope` runs before dispatching
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
        let mut inner = self.inner.lock().expect("task store poisoned");

        let task = inner
            .get_mut(id)
            .ok_or_else(|| TaskStoreError::NotFound(id.to_owned()))?;
        validate_task_update(
            task,
            status.as_ref(),
            result.as_deref(),
            log_msg.as_deref(),
            workspace,
        )?;

        let now = Utc::now();
        let mut changed = false;

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
        // the caller (record_dispatch is best-effort in Go too).
        let _ = self.persist_locked(&inner);
        Some(snapshot)
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
    Ok(())
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
