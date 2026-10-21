//! BYOA/ACP runtime wiring for the headless host.
//!
//! `agentero-host` owns the whole agent domain and talks to the outside world
//! through three seams (`HostSink`, `Host::spawn`, `Host::config_dir`). The web
//! host implements the sink over its WebSocket event bus so `agent:*` events
//! reach the browser tab that started the turn; everything else uses the
//! crate's `LocalHost` defaults (tokio spawn + XDG config dir + PATH launch).

use std::sync::Arc;

use agentero_host::host::{Host, HostSink, LocalHost};
use agentero_host::service::AgentHost;
use serde_json::Value;

use crate::events::EventBus;

/// `HostSink` that forwards agent events into the web event bus.
#[derive(Clone)]
pub struct EventBusSink {
    bus: EventBus,
}

impl EventBusSink {
    pub fn new(bus: EventBus) -> Self {
        Self { bus }
    }
}

impl HostSink for EventBusSink {
    fn emit(&self, event: &str, payload: Value, target: Option<String>) {
        self.bus.emit_value(event, payload, target);
    }
}

/// Build the agent runtime backed by this process.
///
/// Bundled ACP adapters are resolved from `AGENTERO_RESOURCE_DIR/adapters` when
/// that directory exists (the Docker image bakes them there); otherwise the
/// registry falls back to PATH-installed adapters and the CLIs on the server.
pub fn build_agent_host(bus: EventBus) -> Arc<AgentHost> {
    let sink: Arc<dyn HostSink> = Arc::new(EventBusSink::new(bus));
    let mut host = LocalHost::with_sink(sink);
    if let Some(dir) = adapters_dir() {
        host = host.with_resource_dir(dir);
    }
    Arc::new(AgentHost::new(Arc::new(host) as Arc<dyn Host>))
}

fn adapters_dir() -> Option<std::path::PathBuf> {
    let root = std::env::var_os("AGENTERO_RESOURCE_DIR").map(std::path::PathBuf::from)?;
    let adapters = root.join("adapters");
    adapters.is_dir().then_some(adapters)
}
