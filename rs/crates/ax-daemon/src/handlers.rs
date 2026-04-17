//! Envelope dispatch. Each handler takes a decoded payload, mutates
//! the shared state (registry + queue), and returns the response
//! envelope the server should write back.
//!
//! Port of `internal/daemon/daemon_handlers.go` restricted to the MVP
//! handler set. Persistence side-effects, task-store dispatch, and
//! session-manager wake ensuring will land in later slices.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use ax_proto::payloads::{
    AgentLifecyclePayload, BroadcastPayload, ControlLifecyclePayload, GetSharedPayload,
    ReadMessagesPayload, RecallMemoriesPayload, RegisterPayload, RememberMemoryPayload,
    SendMessagePayload, SetSharedPayload, SetStatusPayload,
};
use ax_proto::responses::{
    AgentLifecycleResponse, BroadcastResponse, ControlLifecycleResponse, GetSharedResponse,
    ListSharedResponse, ListWorkspacesResponse, MemoryResponse, ReadMessagesResponse,
    RecallMemoriesResponse, SendMessageResponse, StatusResponse,
};
use ax_proto::types::{LifecycleAction, Message};
use ax_proto::{Envelope, ErrorPayload, MessageType, ResponsePayload};
use ax_workspace::{
    cleanup_orchestrator_state, dispatch_runnable_work, ensure_orchestrator,
    load_dispatch_desired_state, restart_named_target, start_named_target, stop_named_target,
    DesiredOrchestrator, DesiredWorkspace, DispatchBackend, Manager, RealTmux, TmuxBackend,
};

use crate::memory::{Query as MemoryQuery, Store as MemoryStore};
use crate::queue::MessageQueue;
use crate::registry::{Entry, RegisterOutcome, Registry};
use crate::shared_values::SharedValues;

/// Context shared across handlers for one connected client.
pub(crate) struct HandlerCtx {
    pub socket_path: PathBuf,
    pub registry: Arc<Registry>,
    pub queue: Arc<MessageQueue>,
    pub shared: Arc<SharedValues>,
    pub memory: Arc<MemoryStore>,
}

pub(crate) struct RegisterHandled {
    pub response: Envelope,
    pub outcome: RegisterOutcome,
}

pub(crate) fn handle_register(
    ctx: &HandlerCtx,
    env: &Envelope,
) -> Result<RegisterHandled, HandlerError> {
    let payload: RegisterPayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("register", e))?;
    let outcome = ctx.registry.register(
        &payload.workspace,
        &payload.dir,
        &payload.description,
        &payload.config_path,
    );
    let response = response(
        &env.id,
        &StatusResponse {
            status: "registered".into(),
        },
    )?;
    Ok(RegisterHandled { response, outcome })
}

pub(crate) fn handle_unregister(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
) -> Result<Envelope, HandlerError> {
    if !workspace.is_empty() {
        ctx.registry.unregister(workspace);
    }
    response(
        &env.id,
        &StatusResponse {
            status: "unregistered".into(),
        },
    )
}

pub(crate) fn handle_list_workspaces(
    ctx: &HandlerCtx,
    env: &Envelope,
) -> Result<Envelope, HandlerError> {
    response(
        &env.id,
        &ListWorkspacesResponse {
            workspaces: ctx.registry.list(),
        },
    )
}

pub(crate) fn handle_set_status(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
) -> Result<Envelope, HandlerError> {
    let payload: SetStatusPayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("set_status", e))?;
    require_registered(workspace)?;
    ctx.registry.set_status_text(workspace, &payload.status);
    response(
        &env.id,
        &StatusResponse {
            status: "ok".into(),
        },
    )
}

pub(crate) fn handle_control_lifecycle(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
) -> Result<Envelope, HandlerError> {
    let ax_bin = current_ax_bin()?;
    handle_control_lifecycle_with_tmux(ctx, env, workspace, &RealTmux, &ax_bin)
}

pub(crate) fn handle_agent_lifecycle(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
) -> Result<Envelope, HandlerError> {
    let ax_bin = current_ax_bin()?;
    handle_agent_lifecycle_with_tmux(ctx, env, workspace, &RealTmux, &ax_bin)
}

pub(crate) fn handle_send_message(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
) -> Result<Envelope, HandlerError> {
    let ax_bin = current_ax_bin()?;
    handle_send_message_with_dispatch(ctx, env, workspace, &RealTmux, &ax_bin)
}

fn handle_send_message_with_dispatch<B: DispatchBackend + Clone>(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
    dispatch: &B,
    ax_bin: &Path,
) -> Result<Envelope, HandlerError> {
    let payload: SendMessagePayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("send_message", e))?;
    require_registered(workspace)?;
    if payload.to == workspace {
        return Err(HandlerError::Logic("cannot send message to self".into()));
    }

    let msg = Message {
        id: format!("msg-{}", Uuid::new_v4()),
        from: workspace.to_owned(),
        to: payload.to.clone(),
        content: payload.message.clone(),
        task_id: String::new(),
        created_at: Utc::now(),
    };
    let msg = ctx.queue.enqueue(msg);
    ctx.registry.touch(workspace, msg.created_at);
    push_if_registered(ctx, &payload.to, &msg);

    let config_path = payload.config_path.trim();
    if !config_path.is_empty() {
        dispatch_runnable_work(
            dispatch,
            &ctx.socket_path,
            Path::new(config_path),
            ax_bin,
            &payload.to,
            workspace,
            false,
        )
        .map_err(|e| HandlerError::Logic(format!("dispatch {workspace} -> {}: {e}", payload.to)))?;
    }

    response(
        &env.id,
        &SendMessageResponse {
            message_id: msg.id,
            status: "sent".into(),
        },
    )
}

pub(crate) fn handle_broadcast(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
) -> Result<Envelope, HandlerError> {
    let ax_bin = current_ax_bin()?;
    handle_broadcast_with_dispatch(ctx, env, workspace, &RealTmux, &ax_bin)
}

fn handle_broadcast_with_dispatch<B: DispatchBackend + Clone>(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
    dispatch: &B,
    ax_bin: &Path,
) -> Result<Envelope, HandlerError> {
    let payload: BroadcastPayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("broadcast", e))?;
    require_registered(workspace)?;

    let mut recipients = Vec::new();
    for ws in ctx.registry.list() {
        if ws.name == workspace {
            continue;
        }
        let msg = Message {
            id: format!("msg-{}", Uuid::new_v4()),
            from: workspace.to_owned(),
            to: ws.name.clone(),
            content: payload.message.clone(),
            task_id: String::new(),
            created_at: Utc::now(),
        };
        let msg = ctx.queue.enqueue(msg);
        recipients.push(ws.name.clone());
        push_if_registered(ctx, &ws.name, &msg);
    }
    ctx.registry.touch(workspace, Utc::now());

    let config_path = payload.config_path.trim();
    if !config_path.is_empty() {
        for recipient in &recipients {
            dispatch_runnable_work(
                dispatch,
                &ctx.socket_path,
                Path::new(config_path),
                ax_bin,
                recipient,
                workspace,
                false,
            )
            .map_err(|e| {
                HandlerError::Logic(format!(
                    "broadcast dispatch {workspace} -> {recipient}: {e}"
                ))
            })?;
        }
    }

    let count = i64::try_from(recipients.len()).unwrap_or(i64::MAX);
    response(&env.id, &BroadcastResponse { recipients, count })
}

pub(crate) fn handle_read_messages(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
) -> Result<Envelope, HandlerError> {
    let payload: ReadMessagesPayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("read_messages", e))?;
    require_registered(workspace)?;

    let limit = if payload.limit <= 0 {
        10
    } else {
        usize::try_from(payload.limit).unwrap_or(10)
    };
    let from = (!payload.from.is_empty()).then_some(payload.from.as_str());
    let messages = ctx.queue.dequeue(workspace, limit, from);
    if !messages.is_empty() {
        ctx.registry.touch(workspace, Utc::now());
    }
    response(&env.id, &ReadMessagesResponse { messages })
}

pub(crate) fn handle_set_shared(
    ctx: &HandlerCtx,
    env: &Envelope,
) -> Result<Envelope, HandlerError> {
    let payload: SetSharedPayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("set_shared", e))?;
    ctx.shared
        .set(&payload.key, &payload.value)
        .map_err(|e| HandlerError::Logic(format!("persist shared values: {e}")))?;
    response(
        &env.id,
        &StatusResponse {
            status: "stored".into(),
        },
    )
}

pub(crate) fn handle_get_shared(
    ctx: &HandlerCtx,
    env: &Envelope,
) -> Result<Envelope, HandlerError> {
    let payload: GetSharedPayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("get_shared", e))?;
    let value = ctx.shared.get(&payload.key);
    let found = value.is_some();
    response(
        &env.id,
        &GetSharedResponse {
            key: payload.key,
            value: value.unwrap_or_default(),
            found,
        },
    )
}

pub(crate) fn handle_list_shared(
    ctx: &HandlerCtx,
    env: &Envelope,
) -> Result<Envelope, HandlerError> {
    response(
        &env.id,
        &ListSharedResponse {
            values: ctx.shared.list(),
        },
    )
}

pub(crate) fn handle_remember_memory(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
) -> Result<Envelope, HandlerError> {
    let payload: RememberMemoryPayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("remember_memory", e))?;
    require_registered(workspace)?;
    let memory = ctx
        .memory
        .remember(
            &payload.scope,
            &payload.kind,
            &payload.subject,
            &payload.content,
            &payload.tags,
            workspace,
            &payload.supersedes,
        )
        .map_err(|e| HandlerError::Logic(e.to_string()))?;
    ctx.registry.touch(workspace, Utc::now());
    response(&env.id, &MemoryResponse { memory })
}

pub(crate) fn handle_recall_memories(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
) -> Result<Envelope, HandlerError> {
    let payload: RecallMemoriesPayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("recall_memories", e))?;
    require_registered(workspace)?;
    let memories = ctx.memory.list(&MemoryQuery {
        scopes: payload.scopes,
        kind: payload.kind,
        tags: payload.tags,
        include_superseded: payload.include_superseded,
        limit: payload.limit,
    });
    ctx.registry.touch(workspace, Utc::now());
    response(&env.id, &RecallMemoriesResponse { memories })
}

// ---------- helpers ----------

fn push_if_registered(ctx: &HandlerCtx, target: &str, msg: &Message) {
    if let Some(entry) = ctx.registry.get(target) {
        if let Ok(push) = Envelope::new(String::new(), MessageType::PushMessage, msg) {
            let _ = entry.try_send(push);
        }
    }
}

fn require_registered(workspace: &str) -> Result<(), HandlerError> {
    if workspace.is_empty() {
        return Err(HandlerError::Logic("not registered".into()));
    }
    Ok(())
}

fn handle_control_lifecycle_with_tmux<B: TmuxBackend + Clone>(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
    tmux: &B,
    ax_bin: &Path,
) -> Result<Envelope, HandlerError> {
    let payload: ControlLifecyclePayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("control_lifecycle", e))?;
    require_registered(workspace)?;

    let config_path = payload.config_path.trim();
    if config_path.is_empty() {
        return Err(HandlerError::Logic("config_path is required".into()));
    }
    let target_name = payload.name.trim();
    if target_name.is_empty() {
        return Err(HandlerError::Logic("name is required".into()));
    }

    let action = payload.action;
    let target = control_lifecycle_target(
        tmux,
        &ctx.socket_path,
        Path::new(config_path),
        ax_bin,
        target_name,
        &action,
    )?;
    ctx.registry.touch(workspace, Utc::now());

    response(
        &env.id,
        &ControlLifecycleResponse {
            target,
            running: !matches!(action, LifecycleAction::Stop),
            action,
        },
    )
}

#[derive(Debug, Clone)]
struct ResolvedAgentLifecycleTarget {
    name: String,
    kind: &'static str,
    managed_session: bool,
    workspace: Option<DesiredWorkspace>,
    orchestrator: Option<DesiredOrchestrator>,
    limit: Option<String>,
}

fn control_lifecycle_target<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target_name: &str,
    action: &LifecycleAction,
) -> Result<ax_proto::types::LifecycleTarget, HandlerError> {
    match action {
        LifecycleAction::Start => {
            start_named_target(tmux, socket_path, config_path, ax_bin, target_name)
        }
        LifecycleAction::Stop => {
            stop_named_target(tmux, socket_path, config_path, ax_bin, target_name)
        }
        LifecycleAction::Restart => {
            restart_named_target(tmux, socket_path, config_path, ax_bin, target_name)
        }
    }
    .map_err(|e| HandlerError::Logic(e.to_string()))
}

fn handle_agent_lifecycle_with_tmux<B: TmuxBackend + Clone>(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
    tmux: &B,
    ax_bin: &Path,
) -> Result<Envelope, HandlerError> {
    let payload: AgentLifecyclePayload = env
        .decode_payload()
        .map_err(|e| HandlerError::DecodePayload("agent_lifecycle", e))?;
    require_registered(workspace)?;

    let config_path = payload.config_path.trim();
    if config_path.is_empty() {
        return Err(HandlerError::Logic("config_path is required".into()));
    }

    let target =
        resolve_agent_lifecycle_target(&ctx.socket_path, Path::new(config_path), &payload.name)?;
    let result = apply_agent_lifecycle_action(
        tmux,
        &ctx.socket_path,
        Path::new(config_path),
        ax_bin,
        target,
        &payload.action,
    )?;
    ctx.registry.touch(workspace, Utc::now());
    response(&env.id, &result)
}

fn resolve_agent_lifecycle_target(
    socket_path: &Path,
    config_path: &Path,
    name: &str,
) -> Result<ResolvedAgentLifecycleTarget, HandlerError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(HandlerError::Logic("name is required".into()));
    }

    let desired = load_dispatch_desired_state(socket_path, config_path)
        .map_err(|e| HandlerError::Logic(e.to_string()))?;

    if let Some(entry) = desired.workspaces.get(name) {
        return Ok(ResolvedAgentLifecycleTarget {
            name: entry.name.clone(),
            kind: "workspace",
            managed_session: true,
            workspace: Some(entry.clone()),
            orchestrator: None,
            limit: None,
        });
    }

    if let Some(entry) = desired.orchestrators.get(name) {
        return Ok(ResolvedAgentLifecycleTarget {
            name: entry.name.clone(),
            kind: "orchestrator",
            managed_session: entry.managed_session,
            workspace: None,
            orchestrator: Some(entry.clone()),
            limit: (entry.root || !entry.managed_session).then(|| {
                "root orchestrator lifecycle is not supported here because it is not a daemon-managed session"
                    .to_owned()
            }),
        });
    }

    Err(HandlerError::Logic(format!(
        "Agent {name:?} is not defined exactly in {}; use list_agents for exact configured names",
        config_path.display()
    )))
}

fn apply_agent_lifecycle_action<B: TmuxBackend + Clone>(
    tmux: &B,
    socket_path: &Path,
    config_path: &Path,
    ax_bin: &Path,
    target: ResolvedAgentLifecycleTarget,
    action: &LifecycleAction,
) -> Result<AgentLifecycleResponse, HandlerError> {
    if let Some(limit) = target.limit.as_deref() {
        return Err(HandlerError::Logic(format!(
            "Agent {:?} does not support {}: {}",
            target.name,
            lifecycle_action_name(action),
            limit
        )));
    }

    let existed_before = tmux.session_exists(&target.name);
    let mut result = AgentLifecycleResponse {
        name: target.name.clone(),
        action: lifecycle_action_name(action).to_owned(),
        target_kind: target.kind.to_owned(),
        managed_session: target.managed_session,
        exact_match: true,
        status: String::new(),
        session_exists_before: existed_before,
        session_exists_after: false,
    };

    match target.kind {
        "workspace" => {
            let workspace = target.workspace.as_ref().expect("workspace target");
            let manager = Manager::with_tmux(
                socket_path.to_path_buf(),
                Some(config_path.to_path_buf()),
                ax_bin.to_path_buf(),
                tmux.clone(),
            );
            match action {
                LifecycleAction::Start => {
                    if existed_before {
                        result.status = "already_running".to_owned();
                    } else {
                        manager
                            .create(&target.name, &workspace.workspace)
                            .map_err(|e| {
                                HandlerError::Logic(format!(
                                    "start workspace {:?}: {}",
                                    target.name, e
                                ))
                            })?;
                        result.status = "started".to_owned();
                    }
                }
                LifecycleAction::Stop => {
                    manager
                        .destroy(&target.name, &workspace.workspace.dir)
                        .map_err(|e| {
                            HandlerError::Logic(format!("stop workspace {:?}: {}", target.name, e))
                        })?;
                    result.status = if existed_before {
                        "stopped".to_owned()
                    } else {
                        "already_stopped".to_owned()
                    };
                }
                LifecycleAction::Restart => {
                    manager
                        .restart(&target.name, &workspace.workspace)
                        .map_err(|e| {
                            HandlerError::Logic(format!(
                                "restart workspace {:?}: {}",
                                target.name, e
                            ))
                        })?;
                    result.status = "restarted".to_owned();
                }
            }
        }
        "orchestrator" => {
            let orchestrator = target.orchestrator.as_ref().expect("orchestrator target");
            match action {
                LifecycleAction::Start => {
                    if existed_before {
                        result.status = "already_running".to_owned();
                    } else {
                        ensure_orchestrator(
                            tmux,
                            &orchestrator.node,
                            &orchestrator.parent_name,
                            socket_path,
                            Some(config_path),
                            ax_bin,
                            true,
                        )
                        .map_err(|e| {
                            HandlerError::Logic(format!(
                                "start orchestrator {:?}: {}",
                                target.name, e
                            ))
                        })?;
                        result.status = "started".to_owned();
                    }
                }
                LifecycleAction::Stop => {
                    cleanup_orchestrator_state(tmux, &target.name, &orchestrator.artifact_dir)
                        .map_err(|e| {
                            HandlerError::Logic(format!(
                                "stop orchestrator {:?}: {}",
                                target.name, e
                            ))
                        })?;
                    result.status = if existed_before {
                        "stopped".to_owned()
                    } else {
                        "already_stopped".to_owned()
                    };
                }
                LifecycleAction::Restart => {
                    cleanup_orchestrator_state(tmux, &target.name, &orchestrator.artifact_dir)
                        .map_err(|e| {
                            HandlerError::Logic(format!(
                                "restart orchestrator {:?}: {}",
                                target.name, e
                            ))
                        })?;
                    ensure_orchestrator(
                        tmux,
                        &orchestrator.node,
                        &orchestrator.parent_name,
                        socket_path,
                        Some(config_path),
                        ax_bin,
                        true,
                    )
                    .map_err(|e| {
                        HandlerError::Logic(format!(
                            "restart orchestrator {:?}: {}",
                            target.name, e
                        ))
                    })?;
                    result.status = "restarted".to_owned();
                }
            }
        }
        _ => {
            return Err(HandlerError::Logic(format!(
                "unsupported lifecycle target kind {:?}",
                target.kind
            )));
        }
    }

    result.session_exists_after = tmux.session_exists(&target.name);
    match action {
        LifecycleAction::Start | LifecycleAction::Restart => {
            if !result.session_exists_after {
                return Err(HandlerError::Logic(format!(
                    "{} {:?} completed without leaving a running session",
                    lifecycle_action_name(action),
                    target.name
                )));
            }
        }
        LifecycleAction::Stop => {
            if result.session_exists_after {
                return Err(HandlerError::Logic(format!(
                    "stop {:?} completed but the session is still running",
                    target.name
                )));
            }
        }
    }

    Ok(result)
}

fn lifecycle_action_name(action: &LifecycleAction) -> &'static str {
    match action {
        LifecycleAction::Start => "start",
        LifecycleAction::Stop => "stop",
        LifecycleAction::Restart => "restart",
    }
}

fn current_ax_bin() -> Result<PathBuf, HandlerError> {
    std::env::current_exe()
        .map_err(|e| HandlerError::Logic(format!("resolve current executable: {e}")))
}

pub(crate) fn response_envelope<T: serde::Serialize>(
    id: &str,
    data: &T,
) -> Result<Envelope, HandlerError> {
    response(id, data)
}

fn response<T: serde::Serialize>(id: &str, data: &T) -> Result<Envelope, HandlerError> {
    let data = serde_json::value::RawValue::from_string(serde_json::to_string(data)?)?;
    Envelope::new(
        id,
        MessageType::Response,
        &ResponsePayload {
            success: true,
            data,
        },
    )
    .map_err(HandlerError::Serialize)
}

pub(crate) fn error_envelope(id: &str, message: impl Into<String>) -> Envelope {
    Envelope::new(
        id,
        MessageType::Error,
        &ErrorPayload {
            message: message.into(),
        },
    )
    .expect("serialize error envelope")
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HandlerError {
    #[error("decode {0} payload: {1}")]
    DecodePayload(&'static str, serde_json::Error),
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
    #[error("{0}")]
    Logic(String),
}

pub(crate) fn handle_envelope(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &mut String,
    connection_id: &mut Option<u64>,
) -> HandlerOutput {
    match env.r#type {
        MessageType::Register => match handle_register(ctx, env) {
            Ok(RegisterHandled { response, outcome }) => {
                workspace.clone_from(&outcome.entry.info.name);
                *connection_id = Some(outcome.entry.id);
                HandlerOutput::Registered {
                    response,
                    entry: outcome.entry,
                    receiver: outcome.receiver,
                    previous_outbox: outcome.previous.map(|p| p.outbox),
                }
            }
            Err(e) => HandlerOutput::Response(error_envelope(&env.id, e.to_string())),
        },
        MessageType::Unregister => {
            let ws = workspace.clone();
            let resp = handle_unregister(ctx, env, &ws)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string()));
            workspace.clear();
            *connection_id = None;
            HandlerOutput::Response(resp)
        }
        MessageType::ListWorkspaces => HandlerOutput::Response(
            handle_list_workspaces(ctx, env)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::SetStatus => HandlerOutput::Response(
            handle_set_status(ctx, env, workspace)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::ControlLifecycle => HandlerOutput::Response(
            handle_control_lifecycle(ctx, env, workspace)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::AgentLifecycle => HandlerOutput::Response(
            handle_agent_lifecycle(ctx, env, workspace)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::SendMessage => HandlerOutput::Response(
            handle_send_message(ctx, env, workspace)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::Broadcast => HandlerOutput::Response(
            handle_broadcast(ctx, env, workspace)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::ReadMessages => HandlerOutput::Response(
            handle_read_messages(ctx, env, workspace)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::UsageTrends => HandlerOutput::Response(
            crate::usage_trends::handle_usage_trends(env)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::SetShared => HandlerOutput::Response(
            handle_set_shared(ctx, env).unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::GetShared => HandlerOutput::Response(
            handle_get_shared(ctx, env).unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::ListShared => HandlerOutput::Response(
            handle_list_shared(ctx, env).unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::RememberMemory => HandlerOutput::Response(
            handle_remember_memory(ctx, env, workspace)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        MessageType::RecallMemories => HandlerOutput::Response(
            handle_recall_memories(ctx, env, workspace)
                .unwrap_or_else(|e| error_envelope(&env.id, e.to_string())),
        ),
        _ => HandlerOutput::Response(error_envelope(
            &env.id,
            format!("unknown message type: {:?}", env.r#type),
        )),
    }
}

pub(crate) enum HandlerOutput {
    Response(Envelope),
    Registered {
        response: Envelope,
        entry: Entry,
        receiver: tokio::sync::mpsc::Receiver<Envelope>,
        previous_outbox: Option<tokio::sync::mpsc::Sender<Envelope>>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashSet};
    use std::fs;

    use tempfile::TempDir;

    use ax_proto::{responses::AgentLifecycleResponse, ResponsePayload};
    use ax_workspace::TmuxBackend;

    #[derive(Debug, Default, Clone)]
    struct FakeTmux {
        sessions: Arc<std::sync::Mutex<HashSet<String>>>,
        wakes: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    }

    impl TmuxBackend for FakeTmux {
        fn session_exists(&self, workspace: &str) -> bool {
            self.sessions
                .lock()
                .expect("sessions lock")
                .contains(workspace)
        }

        fn list_sessions(&self) -> Result<Vec<ax_tmux::SessionInfo>, ax_tmux::TmuxError> {
            Ok(Vec::new())
        }

        fn is_idle(&self, _workspace: &str) -> bool {
            true
        }

        fn create_session(
            &self,
            workspace: &str,
            _dir: &str,
            _shell: &str,
            _env: &BTreeMap<String, String>,
        ) -> Result<(), ax_tmux::TmuxError> {
            self.sessions
                .lock()
                .expect("sessions lock")
                .insert(workspace.to_owned());
            Ok(())
        }

        fn create_session_with_command(
            &self,
            workspace: &str,
            _dir: &str,
            _command: &str,
            _env: &BTreeMap<String, String>,
        ) -> Result<(), ax_tmux::TmuxError> {
            self.sessions
                .lock()
                .expect("sessions lock")
                .insert(workspace.to_owned());
            Ok(())
        }

        fn create_session_with_args(
            &self,
            workspace: &str,
            _dir: &str,
            _argv: &[String],
            _env: &BTreeMap<String, String>,
        ) -> Result<(), ax_tmux::TmuxError> {
            self.sessions
                .lock()
                .expect("sessions lock")
                .insert(workspace.to_owned());
            Ok(())
        }

        fn destroy_session(&self, workspace: &str) -> Result<(), ax_tmux::TmuxError> {
            self.sessions
                .lock()
                .expect("sessions lock")
                .remove(workspace);
            Ok(())
        }
    }

    impl DispatchBackend for FakeTmux {
        fn wake_workspace(&self, workspace: &str, prompt: &str) -> Result<(), ax_tmux::TmuxError> {
            self.wakes
                .lock()
                .expect("wakes lock")
                .push((workspace.to_owned(), prompt.to_owned()));
            Ok(())
        }
    }

    fn test_ctx(socket_path: PathBuf) -> HandlerCtx {
        HandlerCtx {
            socket_path,
            registry: Registry::new(),
            queue: MessageQueue::new(),
            shared: SharedValues::in_memory(),
            memory: MemoryStore::in_memory(),
        }
    }

    fn decode_response<T: for<'de> serde::Deserialize<'de>>(env: &Envelope) -> T {
        assert_eq!(env.r#type, MessageType::Response);
        let wrap: ResponsePayload = env.decode_payload().expect("response payload");
        assert!(wrap.success);
        serde_json::from_str(wrap.data.get()).expect("decode response body")
    }

    fn write_config(root: &TempDir, body: &str) -> PathBuf {
        let config_path = root.path().join(".ax").join("config.yaml");
        fs::create_dir_all(config_path.parent().expect("config dir")).expect("create config dir");
        fs::write(&config_path, body).expect("write config");
        config_path
    }

    fn write_child_config(root: &TempDir, child: &str, body: &str) -> PathBuf {
        let config_path = root.path().join(child).join(".ax").join("config.yaml");
        fs::create_dir_all(config_path.parent().expect("child config dir"))
            .expect("create child config dir");
        fs::write(&config_path, body).expect("write child config");
        config_path
    }

    #[test]
    fn control_lifecycle_start_returns_running_target() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("caller", "/tmp/caller", "", "");
        let env = Envelope::new(
            "ctl-1",
            MessageType::ControlLifecycle,
            &ControlLifecyclePayload {
                config_path: config_path.display().to_string(),
                name: "worker".into(),
                action: LifecycleAction::Start,
            },
        )
        .expect("encode envelope");

        let response = handle_control_lifecycle_with_tmux(
            &ctx,
            &env,
            "caller",
            &FakeTmux::default(),
            Path::new("/tmp/ax-rs"),
        )
        .expect("control lifecycle");

        let decoded: ControlLifecycleResponse = decode_response(&response);
        assert_eq!(decoded.target.name, "worker");
        assert!(decoded.running);
        assert_eq!(decoded.action, LifecycleAction::Start);
    }

    #[test]
    fn control_lifecycle_requires_registration() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        let env = Envelope::new(
            "ctl-2",
            MessageType::ControlLifecycle,
            &ControlLifecyclePayload {
                config_path: config_path.display().to_string(),
                name: "worker".into(),
                action: LifecycleAction::Start,
            },
        )
        .expect("encode envelope");

        let err = handle_control_lifecycle_with_tmux(
            &ctx,
            &env,
            "",
            &FakeTmux::default(),
            Path::new("/tmp/ax-rs"),
        )
        .expect_err("missing registration should fail");
        assert_eq!(err.to_string(), "not registered");
    }

    #[test]
    fn control_lifecycle_requires_config_path() {
        let root = TempDir::new().expect("tempdir");
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("caller", "/tmp/caller", "", "");
        let env = Envelope::new(
            "ctl-3",
            MessageType::ControlLifecycle,
            &ControlLifecyclePayload {
                config_path: String::new(),
                name: "worker".into(),
                action: LifecycleAction::Start,
            },
        )
        .expect("encode envelope");

        let err = handle_control_lifecycle_with_tmux(
            &ctx,
            &env,
            "caller",
            &FakeTmux::default(),
            Path::new("/tmp/ax-rs"),
        )
        .expect_err("missing config should fail");
        assert_eq!(err.to_string(), "config_path is required");
    }

    #[test]
    fn control_lifecycle_propagates_target_errors() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\norchestrator_runtime: claude\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("caller", "/tmp/caller", "", "");
        let env = Envelope::new(
            "ctl-4",
            MessageType::ControlLifecycle,
            &ControlLifecyclePayload {
                config_path: config_path.display().to_string(),
                name: "orchestrator".into(),
                action: LifecycleAction::Stop,
            },
        )
        .expect("encode envelope");

        let err = handle_control_lifecycle_with_tmux(
            &ctx,
            &env,
            "caller",
            &FakeTmux::default(),
            Path::new("/tmp/ax-rs"),
        )
        .expect_err("root orchestrator should fail");
        assert_eq!(
            err.to_string(),
            "orchestrator \"orchestrator\" does not support targeted stop because it is not a managed session"
        );
    }

    #[test]
    fn agent_lifecycle_starts_workspace_by_exact_name() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("caller", "/tmp/caller", "", "");
        let env = Envelope::new(
            "agent-1",
            MessageType::AgentLifecycle,
            &AgentLifecyclePayload {
                config_path: config_path.display().to_string(),
                name: "worker".into(),
                action: LifecycleAction::Start,
            },
        )
        .expect("encode envelope");

        let response = handle_agent_lifecycle_with_tmux(
            &ctx,
            &env,
            "caller",
            &FakeTmux::default(),
            Path::new("/tmp/ax-rs"),
        )
        .expect("agent lifecycle");

        let decoded: AgentLifecycleResponse = decode_response(&response);
        assert_eq!(decoded.name, "worker");
        assert_eq!(decoded.action, "start");
        assert_eq!(decoded.target_kind, "workspace");
        assert!(decoded.managed_session);
        assert!(decoded.exact_match);
        assert_eq!(decoded.status, "started");
        assert!(!decoded.session_exists_before);
        assert!(decoded.session_exists_after);
    }

    #[test]
    fn agent_lifecycle_start_reports_already_running() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("caller", "/tmp/caller", "", "");
        let tmux = FakeTmux::default();
        tmux.sessions
            .lock()
            .expect("sessions lock")
            .insert("worker".to_owned());
        let env = Envelope::new(
            "agent-2",
            MessageType::AgentLifecycle,
            &AgentLifecyclePayload {
                config_path: config_path.display().to_string(),
                name: "worker".into(),
                action: LifecycleAction::Start,
            },
        )
        .expect("encode envelope");

        let response =
            handle_agent_lifecycle_with_tmux(&ctx, &env, "caller", &tmux, Path::new("/tmp/ax-rs"))
                .expect("agent lifecycle");

        let decoded: AgentLifecycleResponse = decode_response(&response);
        assert_eq!(decoded.status, "already_running");
        assert!(decoded.session_exists_before);
        assert!(decoded.session_exists_after);
    }

    #[test]
    fn agent_lifecycle_starts_managed_child_orchestrator() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\norchestrator_runtime: claude\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\nchildren:\n  child:\n    dir: ./child\n    prefix: team\n",
        );
        let _child_config = write_child_config(
            &root,
            "child",
            "project: child\nworkspaces:\n  helper:\n    dir: ./helper\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("caller", "/tmp/caller", "", "");
        let env = Envelope::new(
            "agent-3",
            MessageType::AgentLifecycle,
            &AgentLifecyclePayload {
                config_path: config_path.display().to_string(),
                name: "team.orchestrator".into(),
                action: LifecycleAction::Start,
            },
        )
        .expect("encode envelope");

        let response = handle_agent_lifecycle_with_tmux(
            &ctx,
            &env,
            "caller",
            &FakeTmux::default(),
            Path::new("/tmp/ax-rs"),
        )
        .expect("agent lifecycle");

        let decoded: AgentLifecycleResponse = decode_response(&response);
        assert_eq!(decoded.name, "team.orchestrator");
        assert_eq!(decoded.target_kind, "orchestrator");
        assert!(decoded.managed_session);
        assert_eq!(decoded.status, "started");
        assert!(decoded.session_exists_after);
    }

    #[test]
    fn agent_lifecycle_rejects_root_orchestrator() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\norchestrator_runtime: claude\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("caller", "/tmp/caller", "", "");
        let env = Envelope::new(
            "agent-4",
            MessageType::AgentLifecycle,
            &AgentLifecyclePayload {
                config_path: config_path.display().to_string(),
                name: "orchestrator".into(),
                action: LifecycleAction::Stop,
            },
        )
        .expect("encode envelope");

        let err = handle_agent_lifecycle_with_tmux(
            &ctx,
            &env,
            "caller",
            &FakeTmux::default(),
            Path::new("/tmp/ax-rs"),
        )
        .expect_err("root orchestrator should fail");
        assert_eq!(
            err.to_string(),
            "Agent \"orchestrator\" does not support stop: root orchestrator lifecycle is not supported here because it is not a daemon-managed session"
        );
    }

    #[test]
    fn agent_lifecycle_requires_exact_configured_name() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("caller", "/tmp/caller", "", "");
        let env = Envelope::new(
            "agent-5",
            MessageType::AgentLifecycle,
            &AgentLifecyclePayload {
                config_path: config_path.display().to_string(),
                name: "Worker".into(),
                action: LifecycleAction::Start,
            },
        )
        .expect("encode envelope");

        let err = handle_agent_lifecycle_with_tmux(
            &ctx,
            &env,
            "caller",
            &FakeTmux::default(),
            Path::new("/tmp/ax-rs"),
        )
        .expect_err("name mismatch should fail");
        assert_eq!(
            err.to_string(),
            format!(
                "Agent \"Worker\" is not defined exactly in {}; use list_agents for exact configured names",
                config_path.display()
            )
        );
    }

    #[test]
    fn send_message_dispatches_when_config_path_is_present() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("orchestrator", "/tmp/orch", "", "");
        let tmux = FakeTmux::default();
        let env = Envelope::new(
            "send-1",
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "worker".into(),
                message: "ping".into(),
                config_path: config_path.display().to_string(),
            },
        )
        .expect("encode envelope");

        let response = handle_send_message_with_dispatch(
            &ctx,
            &env,
            "orchestrator",
            &tmux,
            Path::new("/tmp/ax-rs"),
        )
        .expect("send message");

        let decoded: SendMessageResponse = decode_response(&response);
        assert_eq!(decoded.status, "sent");
        assert_eq!(ctx.queue.pending_count("worker"), 1);
        assert!(tmux.session_exists("worker"));
        let wakes = tmux.wakes.lock().expect("wakes lock");
        assert_eq!(wakes.len(), 1);
        assert_eq!(wakes[0].0, "worker");
        assert!(wakes[0].1.contains(r#"send_message(to="orchestrator")"#));
    }

    #[test]
    fn send_message_skips_dispatch_when_config_path_is_empty() {
        let root = TempDir::new().expect("tempdir");
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("orchestrator", "/tmp/orch", "", "");
        let tmux = FakeTmux::default();
        let env = Envelope::new(
            "send-2",
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "worker".into(),
                message: "ping".into(),
                config_path: String::new(),
            },
        )
        .expect("encode envelope");

        let response = handle_send_message_with_dispatch(
            &ctx,
            &env,
            "orchestrator",
            &tmux,
            Path::new("/tmp/ax-rs"),
        )
        .expect("send message");

        let decoded: SendMessageResponse = decode_response(&response);
        assert_eq!(decoded.status, "sent");
        assert_eq!(ctx.queue.pending_count("worker"), 1);
        assert!(!tmux.session_exists("worker"));
        assert!(tmux.wakes.lock().expect("wakes lock").is_empty());
    }

    #[test]
    fn send_message_keeps_message_queued_when_dispatch_fails() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\nworkspaces:\n  worker:\n    dir: ./worker\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("orchestrator", "/tmp/orch", "", "");
        let tmux = FakeTmux::default();
        let env = Envelope::new(
            "send-3",
            MessageType::SendMessage,
            &SendMessagePayload {
                to: "missing".into(),
                message: "ping".into(),
                config_path: config_path.display().to_string(),
            },
        )
        .expect("encode envelope");

        let err = handle_send_message_with_dispatch(
            &ctx,
            &env,
            "orchestrator",
            &tmux,
            Path::new("/tmp/ax-rs"),
        )
        .expect_err("dispatch should fail");
        assert!(err.to_string().contains(
            r#"dispatch orchestrator -> missing: dispatch target "missing" is not defined"#
        ));
        assert_eq!(ctx.queue.pending_count("missing"), 1);
    }

    #[test]
    fn broadcast_dispatches_to_each_recipient_when_config_path_is_present() {
        let root = TempDir::new().expect("tempdir");
        let config_path = write_config(
            &root,
            "project: demo\nworkspaces:\n  worker-a:\n    dir: ./worker-a\n    runtime: claude\n  worker-b:\n    dir: ./worker-b\n    runtime: claude\n",
        );
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("orchestrator", "/tmp/orch", "", "");
        ctx.registry.register("worker-a", "/tmp/a", "", "");
        ctx.registry.register("worker-b", "/tmp/b", "", "");
        let tmux = FakeTmux::default();
        let env = Envelope::new(
            "broadcast-1",
            MessageType::Broadcast,
            &BroadcastPayload {
                message: "team notice".into(),
                config_path: config_path.display().to_string(),
            },
        )
        .expect("encode envelope");

        let response = handle_broadcast_with_dispatch(
            &ctx,
            &env,
            "orchestrator",
            &tmux,
            Path::new("/tmp/ax-rs"),
        )
        .expect("broadcast");

        let decoded: BroadcastResponse = decode_response(&response);
        assert_eq!(decoded.count, 2);
        assert_eq!(ctx.queue.pending_count("worker-a"), 1);
        assert_eq!(ctx.queue.pending_count("worker-b"), 1);
        let mut wakes = tmux.wakes.lock().expect("wakes lock").clone();
        wakes.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(wakes.len(), 2);
        assert_eq!(wakes[0].0, "worker-a");
        assert_eq!(wakes[1].0, "worker-b");
    }

    #[test]
    fn broadcast_skips_dispatch_when_config_path_is_empty() {
        let root = TempDir::new().expect("tempdir");
        let ctx = test_ctx(root.path().join("daemon.sock"));
        ctx.registry.register("orchestrator", "/tmp/orch", "", "");
        ctx.registry.register("worker-a", "/tmp/a", "", "");
        let tmux = FakeTmux::default();
        let env = Envelope::new(
            "broadcast-2",
            MessageType::Broadcast,
            &BroadcastPayload {
                message: "team notice".into(),
                config_path: String::new(),
            },
        )
        .expect("encode envelope");

        let response = handle_broadcast_with_dispatch(
            &ctx,
            &env,
            "orchestrator",
            &tmux,
            Path::new("/tmp/ax-rs"),
        )
        .expect("broadcast");

        let decoded: BroadcastResponse = decode_response(&response);
        assert_eq!(decoded.count, 1);
        assert_eq!(ctx.queue.pending_count("worker-a"), 1);
        assert!(tmux.wakes.lock().expect("wakes lock").is_empty());
    }
}
