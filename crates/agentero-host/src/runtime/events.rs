//! Event emitter for the agent domain.
//!
//! Desktop used `AppHandle::emit_to(EventTarget::webview_window(label), …)`.
//! The host crate only knows a [`HostSink`] + an optional target label, so the
//! same call sites work in the desktop shell (label = window) and in the web
//! server (label = browser tab / WebSocket connection).
//!
//! `emit` intentionally returns `()`: call sites already use `let _ = …`, and
//! event delivery is best-effort (a closed tab must not fail an agent turn).

use crate::host::HostSink;
use serde::Serialize;
use std::sync::Arc;

#[derive(Clone)]
pub struct AgentEventEmitter {
    sink: Arc<dyn HostSink>,
    target: Option<String>,
}

impl AgentEventEmitter {
    /// Emit to one target label (desktop `emit_to`).
    pub fn new(sink: Arc<dyn HostSink>, target: impl Into<String>) -> Self {
        Self {
            sink,
            target: Some(target.into()),
        }
    }

    /// Emit without a target (broadcast, e.g. `agent:registry-changed`).
    pub fn broadcast(sink: Arc<dyn HostSink>) -> Self {
        Self { sink, target: None }
    }

    /// Explicit target: `None` = broadcast.
    pub fn with_target(sink: Arc<dyn HostSink>, target: Option<String>) -> Self {
        Self { sink, target }
    }

    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    pub fn sink(&self) -> &Arc<dyn HostSink> {
        &self.sink
    }

    pub fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) {
        let value = match serde_json::to_value(payload) {
            Ok(value) => value,
            Err(error) => {
                log::error!(
                    target: "agentero::agent",
                    "failed to serialize `{event}` payload: {error}"
                );
                return;
            }
        };
        self.sink.emit(event, value, self.target.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::MockSink;

    #[test]
    fn emitter_serializes_payload_and_targets_the_label() {
        let sink = Arc::new(MockSink::new());
        let emitter = AgentEventEmitter::new(sink.clone(), "tab-7");
        emitter.emit(
            "agent:status",
            serde_json::json!({ "sessionId": "s-1", "phase": "starting" }),
        );
        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "agent:status");
        assert_eq!(events[0].target.as_deref(), Some("tab-7"));
        assert_eq!(
            events[0].payload,
            serde_json::json!({ "sessionId": "s-1", "phase": "starting" })
        );
    }

    #[test]
    fn broadcast_emitter_has_no_target() {
        let sink = Arc::new(MockSink::new());
        AgentEventEmitter::broadcast(sink.clone()).emit("agent:registry-changed", ());
        assert_eq!(sink.events()[0].target, None);
    }
}
