//! Envelope dispatch. Each handler takes a decoded payload, mutates
//! the shared state (registry + queue), and returns the response
//! envelope the server should write back.
//!
//! Port of `internal/daemon/daemon_handlers.go` restricted to the MVP
//! handler set. Persistence side-effects, task-store dispatch, and
//! session-manager wake ensuring will land in later slices.

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use ax_proto::payloads::{
    BroadcastPayload, GetSharedPayload, ReadMessagesPayload, RecallMemoriesPayload,
    RegisterPayload, RememberMemoryPayload, SendMessagePayload, SetSharedPayload, SetStatusPayload,
};
use ax_proto::responses::{
    BroadcastResponse, GetSharedResponse, ListSharedResponse, ListWorkspacesResponse,
    MemoryResponse, ReadMessagesResponse, RecallMemoriesResponse, SendMessageResponse,
    StatusResponse,
};
use ax_proto::types::Message;
use ax_proto::{Envelope, ErrorPayload, MessageType, ResponsePayload};

use crate::memory::{Query as MemoryQuery, Store as MemoryStore};
use crate::queue::MessageQueue;
use crate::registry::{Entry, RegisterOutcome, Registry};
use crate::shared_values::SharedValues;

/// Context shared across handlers for one connected client.
pub(crate) struct HandlerCtx {
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

pub(crate) fn handle_send_message(
    ctx: &HandlerCtx,
    env: &Envelope,
    workspace: &str,
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
