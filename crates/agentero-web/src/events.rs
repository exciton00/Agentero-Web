//! Event bus: broadcast channel shared by the WS endpoint and `HostHooks`.
//!
//! Event names and payloads are unchanged from the desktop Host (45 typed
//! events, `src-tauri/src/app/events_contract.rs`). Frames carry an optional
//! `target` (the issuing browser tab label) so per-window events such as
//! `agent:*` and `vault:file-changed` can be routed instead of fanned out.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::state::AppState;

#[derive(Clone, Debug, Serialize)]
pub struct EventFrame {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub payload: Value,
}

/// Fan-out bus for Host events. Cheap to clone (one broadcast sender).
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EventFrame>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(4096);
        Self { tx }
    }

    pub fn emit<T: Serialize>(&self, event: &str, payload: &T) {
        let payload = serde_json::to_value(payload).unwrap_or(Value::Null);
        self.emit_value(event, payload, None);
    }

    pub fn emit_to<T: Serialize>(&self, target: &str, event: &str, payload: &T) {
        let payload = serde_json::to_value(payload).unwrap_or(Value::Null);
        self.emit_value(event, payload, Some(target.to_string()));
    }

    pub fn emit_value(&self, event: &str, payload: Value, target: Option<String>) {
        let frame = EventFrame {
            event: event.to_string(),
            target,
            payload,
        };
        // A send error only means there are no subscribers right now.
        let _ = self.tx.send(frame);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventFrame> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// `HostHooks` implementation for the headless host.
pub struct WebHostHooks {
    pub bus: EventBus,
}

impl agentero_core::app_handle::HostHooks for WebHostHooks {
    fn emit(&self, event: &str, payload: Value) {
        self.bus.emit_value(event, payload, None);
    }
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    /// Browser tab label; returned to the client and used for targeted events.
    pub label: Option<String>,
}

/// `GET /api/events?label=<tab>`
pub async fn ws_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WsQuery>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let label = query.label.unwrap_or_else(|| "main".to_string());
    upgrade.on_upgrade(move |socket| client_loop(state, socket, label))
}

async fn client_loop(state: Arc<AppState>, socket: WebSocket, label: String) {
    let connections = state.connection_open(&label);
    log::debug!("ws connected label={label} connections={connections}");
    let mut rx = state.events.subscribe();
    let (mut sender, mut receiver) = socket.split();

    let hello = json!({
        "event": "$hello",
        "payload": { "label": label },
    });
    if sender
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            frame = rx.recv() => {
                match frame {
                    Ok(frame) => {
                        // Directed events (`emit_to(window_label)`) only reach
                        // the tab that owns the label; broadcast frames have
                        // no target and go everywhere.
                        if frame
                            .target
                            .as_deref()
                            .is_some_and(|target| target != label)
                        {
                            continue;
                        }
                        match serde_json::to_string(&frame) {
                            Ok(text) => {
                                if sender.send(Message::Text(text.into())).await.is_err() {
                                    break;
                                }
                            }
                            Err(error) => log::warn!("event serialize failed: {error}"),
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        log::warn!("ws client lagged, skipped {skipped} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(value) = serde_json::from_str::<Value>(&text) {
                            handle_client_message(&state, &label, value);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        log::debug!("ws receive error: {error}");
                        break;
                    }
                }
            }
        }
    }
    let remaining = state.connection_close(&label);
    log::debug!("ws disconnected label={label} connections={remaining}");
    if remaining == 0 {
        // A transient drop must not kill file watching: only stop after the
        // tab has stayed away for a minute (a reconnect re-registers the label).
        let sweep_state = state.clone();
        let sweep_label = label.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            if sweep_state.connection_count(&sweep_label) == 0 {
                sweep_state.watcher.stop(&sweep_label);
                log::debug!("ws close sweep stopped watcher label={sweep_label}");
            }
        });
    }
}

/// Cross-tab emits from the browser (`workspace:*`, `agent:attach-context`, …).
fn handle_client_message(state: &Arc<AppState>, label: &str, value: Value) {
    if value.get("type").and_then(Value::as_str) != Some("emit") {
        return;
    }
    let Some(event) = value.get("event").and_then(Value::as_str) else {
        return;
    };
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);
    let target = value
        .get("target")
        .and_then(Value::as_str)
        .map(str::to_string);
    if target.as_deref() == Some(label) {
        // Emitting to self: the local listener already ran; skip the echo.
        return;
    }
    state.events.emit_value(event, payload, target);
}
