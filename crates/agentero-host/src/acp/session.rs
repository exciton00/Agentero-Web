//! Minimal single-turn ACP driver: `initialize` → `session/new` (or
//! resume/load) → `session/prompt` with streaming → `session/cancel`.
//!
//! This is the *transport-level* client the web Host needs first: it proves the
//! ported ACP stack runs without Tauri and emits the real `agent:*` events.
//! The desktop orchestration (`session/run.rs`: context injection, warm pool,
//! skills, preferences, history replay) is ported in phase B on top of the same
//! handlers; see `docs/development/webui-agent-port.md`.
//!
//! Event contract (unchanged from desktop):
//! `agent:status` (starting / waiting-model) → `agent:stream` chunks +
//! rich updates (`agent:tool` / `agent:plan` / …) → `agent:completed`, or
//! `agent:failed` on error.

use crate::features::agent::acp::client::{
    acp_terminals, agent_spawn_cwd, agentero_acp_builder, cancelled_payload,
    client_initialize_request, timed_acp_initialize, timed_acp_load_session, timed_acp_request,
    to_acp_agent, wait_for_cancellation,
};
use crate::features::agent::acp::interaction::{
    await_user_permission, permission_response, PermissionPolicy,
};
use crate::features::agent::acp::updates::{
    emit_rich_session_update, is_pi_startup_banner, stream_from_update,
};
use crate::features::agent::models::{
    AgentDescriptor, AgentFailedEvent, AgentResultPayload, AgentStatusEvent, AgentStreamEvent,
    AgentStreamKind, AgentTemplate, PromptImage,
};
use crate::features::agent::runtime::events::AgentEventEmitter;
use crate::features::agent::runtime::gates::PermissionGate;
use crate::host::HostSink;
use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, ImageContent, LoadSessionRequest, NewSessionRequest,
    PromptRequest, RequestPermissionRequest, ResumeSessionRequest, SessionNotification,
    TextContent,
};
use agent_client_protocol::{Agent, ConnectionTo};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// One prompt turn, addressed by Agentero's run/message ids (used for event
/// correlation, matching the desktop contract).
#[derive(Debug, Clone)]
pub struct PromptTurn {
    pub session_id: String,
    pub message_id: String,
    pub prompt: String,
    pub images: Vec<PromptImage>,
    /// Provider session id to continue (`session/resume` or `session/load`).
    pub resume_session_id: Option<String>,
    /// Durable agent id, used for the `session/load` timeout/log label.
    pub provider_agent_id: String,
}

/// Terminal state of a turn.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub session_id: String,
    pub message_id: String,
    pub provider_session_id: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub stop_reason: Option<String>,
    pub cancelled: bool,
}

/// Permission handling for this turn (desktop default: `Restricted`).
pub struct TurnPolicy {
    pub permission_policy: PermissionPolicy,
    pub permission_gate: PermissionGate,
    /// Cancel the turn when this flips to `true`.
    pub cancellation: watch::Receiver<bool>,
    /// Keeps the default channel's sender alive so `changed()` blocks instead
    /// of resolving `Err` (a dropped sender means "cancelled" to the waiter).
    _cancel_guard: Option<watch::Sender<bool>>,
}

impl TurnPolicy {
    /// Safe default: decline every permission request, no cancellation signal.
    pub fn restricted() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            permission_policy: PermissionPolicy::Restricted,
            permission_gate: PermissionGate::new(),
            cancellation: rx,
            _cancel_guard: Some(tx),
        }
    }

    /// Use an external cancellation channel (the caller keeps the sender, e.g.
    /// `AgentRunController::register`).
    pub fn with_cancellation(
        permission_policy: PermissionPolicy,
        permission_gate: PermissionGate,
        cancellation: watch::Receiver<bool>,
    ) -> Self {
        Self {
            permission_policy,
            permission_gate,
            cancellation,
            _cancel_guard: None,
        }
    }
}

/// Run one prompt turn against `desc`, streaming events through `sink`.
///
/// `target` is the window/tab label for targeted events (None = broadcast).
/// The ACP child is launched through [`to_acp_agent`] (PATH → bundled adapter
/// → raw descriptor command, with the vault/scratch cwd policy).
pub async fn run_prompt_turn(
    sink: Arc<dyn HostSink>,
    target: Option<String>,
    desc: &AgentDescriptor,
    vault_path: Option<&str>,
    turn: PromptTurn,
    policy: TurnPolicy,
) -> Result<TurnOutcome, agentero_core::error::AppError> {
    let emitter = AgentEventEmitter::with_target(sink, target);
    let agent_id = desc.id.clone();
    let is_pi = matches!(desc.template, AgentTemplate::Pi);
    let cwd = agent_spawn_cwd(None, vault_path)?;

    emitter.emit(
        "agent:status",
        AgentStatusEvent::starting(turn.session_id.as_str()),
    );

    let terminals = acp_terminals(Some(cwd.clone()));
    let content = Arc::new(Mutex::new(String::new()));
    let thought = Arc::new(Mutex::new(String::new()));

    let acp = match to_acp_agent(desc, Some(&cwd), None) {
        Ok(acp) => acp,
        Err(error) => {
            emitter.emit(
                "agent:failed",
                AgentFailedEvent {
                    session_id: turn.session_id.clone(),
                    error: error.to_string(),
                },
            );
            return Err(error);
        }
    };

    let notification_emitter = emitter.clone();
    let notification_content = content.clone();
    let notification_thought = thought.clone();
    let notification_session = turn.session_id.clone();
    let notification_agent = agent_id.clone();

    let permission_emitter = emitter.clone();
    let permission_gate = policy.permission_gate.clone();
    let permission_session = turn.session_id.clone();
    let permission_policy = policy.permission_policy;

    let turn_session = turn.session_id.clone();
    let turn_message = turn.message_id.clone();
    let result_content = content.clone();
    let result_thought = thought.clone();
    let failure_emitter = emitter.clone();

    let connect = agentero_acp_builder!(terminals)
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                if let Some((chunk, kind)) = stream_from_update(&notification.update) {
                    let drop_banner = is_pi
                        && matches!(kind, AgentStreamKind::Message)
                        && is_pi_startup_banner(&chunk)
                        && notification_content
                            .lock()
                            .is_ok_and(|buffer| buffer.is_empty());
                    if !drop_banner {
                        match kind {
                            AgentStreamKind::Message => {
                                if let Ok(mut buffer) = notification_content.lock() {
                                    buffer.push_str(&chunk);
                                }
                            }
                            AgentStreamKind::Thought => {
                                if let Ok(mut buffer) = notification_thought.lock() {
                                    buffer.push_str(&chunk);
                                }
                            }
                        }
                        notification_emitter.emit(
                            "agent:stream",
                            AgentStreamEvent {
                                session_id: notification_session.clone(),
                                chunk,
                                kind,
                            },
                        );
                    }
                }
                emit_rich_session_update(
                    &notification_emitter,
                    &notification_session,
                    &notification_agent,
                    &notification,
                );
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                let response = match permission_policy {
                    PermissionPolicy::Auto => permission_response(&request, true),
                    PermissionPolicy::Ask => {
                        await_user_permission(
                            &permission_emitter,
                            &permission_gate,
                            &permission_session,
                            &request,
                        )
                        .await
                    }
                    PermissionPolicy::Restricted => permission_response(&request, false),
                };
                let _ = responder.respond(response);
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(acp, {
            let emitter = emitter.clone();
            move |connection: ConnectionTo<Agent>| async move {
                let mut cancellation = policy.cancellation;
                let init = tokio::select! {
                    result = timed_acp_initialize(
                        connection
                            .send_request(client_initialize_request())
                            .block_task(),
                    ) => result?,
                    () = wait_for_cancellation(&mut cancellation) => {
                        emitter.emit(
                            "agent:completed",
                            cancelled_payload(
                                turn_session.clone(),
                                turn_message.clone(),
                                None,
                                &result_content,
                                &result_thought,
                            ),
                        );
                        return Ok(TurnOutcome {
                            session_id: turn_session.clone(),
                            message_id: turn_message.clone(),
                            provider_session_id: String::new(),
                            content: String::new(),
                            reasoning: None,
                            stop_reason: Some("cancelled".to_string()),
                            cancelled: true,
                        });
                    }
                };

                let can_resume = init
                    .agent_capabilities
                    .session_capabilities
                    .resume
                    .is_some();
                let can_load = init.agent_capabilities.load_session;

                let session_id = match turn.resume_session_id.clone() {
                    Some(resume_id) if can_resume => {
                        let requested = resume_id.clone();
                        tokio::select! {
                            result = timed_acp_request(
                                "session/resume",
                                connection
                                    .send_request(ResumeSessionRequest::new(
                                        requested.clone(),
                                        cwd.clone(),
                                    ))
                                    .block_task(),
                            ) => {
                                result?;
                                requested.into()
                            }
                            () = wait_for_cancellation(&mut cancellation) => {
                                return Ok(cancelled_outcome(
                                    &emitter,
                                    &turn_session,
                                    &turn_message,
                                    None,
                                    &result_content,
                                    &result_thought,
                                ));
                            }
                        }
                    }
                    Some(resume_id) if can_load => {
                        let requested = resume_id.clone();
                        tokio::select! {
                            result = timed_acp_load_session(
                                turn.provider_agent_id.as_str(),
                                requested.as_str(),
                                connection
                                    .send_request(LoadSessionRequest::new(
                                        requested.clone(),
                                        cwd.clone(),
                                    ))
                                    .block_task(),
                            ) => {
                                result?;
                                requested.into()
                            }
                            () = wait_for_cancellation(&mut cancellation) => {
                                return Ok(cancelled_outcome(
                                    &emitter,
                                    &turn_session,
                                    &turn_message,
                                    None,
                                    &result_content,
                                    &result_thought,
                                ));
                            }
                        }
                    }
                    Some(resume_id) => {
                        return Err(agent_client_protocol::util::internal_error(format!(
                            "agent does not support continuing session {resume_id} \
                             (no session/resume or session/load capability)"
                        )));
                    }
                    None => {
                        tokio::select! {
                            result = timed_acp_request(
                                "new_session",
                                connection
                                    .send_request(NewSessionRequest::new(cwd.clone()))
                                    .block_task(),
                            ) => result?.session_id,
                            () = wait_for_cancellation(&mut cancellation) => {
                                return Ok(cancelled_outcome(
                                    &emitter,
                                    &turn_session,
                                    &turn_message,
                                    None,
                                    &result_content,
                                    &result_thought,
                                ));
                            }
                        }
                    }
                };

                let mut blocks: Vec<ContentBlock> =
                    vec![ContentBlock::Text(TextContent::new(turn.prompt.clone()))];
                for image in &turn.images {
                    if image.data.trim().is_empty() || image.mime_type.trim().is_empty() {
                        continue;
                    }
                    blocks.push(ContentBlock::Image(ImageContent::new(
                        image.data.clone(),
                        image.mime_type.clone(),
                    )));
                }

                emitter.emit(
                    "agent:status",
                    AgentStatusEvent::waiting_model(turn_session.as_str()),
                );

                let response = tokio::select! {
                    result = connection
                        .send_request(PromptRequest::new(session_id.clone(), blocks))
                        .block_task() => result?,
                    () = wait_for_cancellation(&mut cancellation) => {
                        let _ = connection
                            .send_notification(CancelNotification::new(session_id.clone()));
                        return Ok(cancelled_outcome(
                            &emitter,
                            &turn_session,
                            &turn_message,
                            Some(session_id.to_string()),
                            &result_content,
                            &result_thought,
                        ));
                    }
                };

                let stop_reason = Some(format!("{:?}", response.stop_reason));
                let content = result_content
                    .lock()
                    .map(|buffer| buffer.clone())
                    .unwrap_or_default();
                let reasoning = result_thought
                    .lock()
                    .map(|buffer| buffer.clone())
                    .ok()
                    .filter(|text| !text.is_empty());
                let payload = AgentResultPayload {
                    session_id: turn_session.clone(),
                    message_id: turn_message.clone(),
                    content: content.clone(),
                    reasoning: reasoning.clone(),
                    sources: Vec::new(),
                    stop_reason: stop_reason.clone(),
                    provider_session_id: Some(session_id.to_string()),
                };
                emitter.emit("agent:completed", payload);

                Ok(TurnOutcome {
                    session_id: turn_session.clone(),
                    message_id: turn_message.clone(),
                    provider_session_id: session_id.to_string(),
                    content,
                    reasoning,
                    stop_reason,
                    cancelled: false,
                })
            }
        });

    match connect.await {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            let message = error.to_string();
            failure_emitter.emit(
                "agent:failed",
                AgentFailedEvent {
                    session_id: turn.session_id.clone(),
                    error: message.clone(),
                },
            );
            Err(agentero_core::error::AppError::message(format!(
                "agent turn failed: {message}"
            )))
        }
    }
}

/// Build the `agent:completed` payload for a cancelled turn and return it.
#[allow(clippy::too_many_arguments)]
fn cancelled_outcome(
    emitter: &AgentEventEmitter,
    session_id: &str,
    message_id: &str,
    provider_session_id: Option<String>,
    content: &Arc<Mutex<String>>,
    thought: &Arc<Mutex<String>>,
) -> TurnOutcome {
    let payload = cancelled_payload(
        session_id.to_string(),
        message_id.to_string(),
        provider_session_id.clone(),
        content,
        thought,
    );
    emitter.emit("agent:completed", payload.clone());
    TurnOutcome {
        session_id: session_id.to_string(),
        message_id: message_id.to_string(),
        provider_session_id: provider_session_id.unwrap_or_default(),
        content: payload.content,
        reasoning: payload.reasoning,
        stop_reason: payload.stop_reason,
        cancelled: true,
    }
}

/// Resolve the cwd for a one-off probe/list call (shared with the desktop
/// `agent_spawn_cwd` policy: vault when present, scratch otherwise).
pub fn resolve_cwd(vault_path: Option<&str>) -> Result<PathBuf, agentero_core::error::AppError> {
    agent_spawn_cwd(None, vault_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::MockSink;

    #[test]
    fn restricted_policy_starts_uncancelled() {
        let policy = TurnPolicy::restricted();
        assert!(!*policy.cancellation.borrow());
        assert!(policy._cancel_guard.is_some());
        assert_eq!(resolve_cwd(None).unwrap().file_name().unwrap(), "agent-cwd");
    }

    #[test]
    fn turn_outcome_carries_the_contract_ids() {
        let outcome = TurnOutcome {
            session_id: "s".into(),
            message_id: "m".into(),
            provider_session_id: "p".into(),
            content: "hi".into(),
            reasoning: None,
            stop_reason: Some("EndTurn".into()),
            cancelled: false,
        };
        assert_eq!(outcome.session_id, "s");
        let sink = Arc::new(MockSink::new());
        let emitter = AgentEventEmitter::new(sink.clone(), "tab");
        emitter.emit(
            "agent:failed",
            AgentFailedEvent {
                session_id: "s".into(),
                error: "boom".into(),
            },
        );
        assert_eq!(sink.event_names(), ["agent:failed"]);
    }
}
