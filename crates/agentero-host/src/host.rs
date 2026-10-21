//! Host abstraction for the BYOA/ACP agent domain.
//!
//! The desktop Host used to hand `tauri::AppHandle` / `WebviewWindow` directly to
//! the agent runtime. The headless web Host cannot: there is no window, no
//! event loop and no Tauri state container. This module is the single seam the
//! domain talks to instead:
//!
//! - [`HostSink`] — emit an `agent:*` / `agent-lifecycle:*` event to a target
//!   (webview label on desktop, WebSocket window label on web) or broadcast.
//! - [`Host::spawn`] — background task (agent turns, installs, warm-ups).
//! - [`Host::config_dir`] — where `agents.json` lives.
//! - [`Host::resource_dir`] — bundled ACP adapters (offline fallback tier).
//! - [`Host::spawn_agent_process`] — resolve how an ACP child is launched on
//!   this host (local `exec`, container `docker exec`, remote `ssh`, …).
//!
//! [`MockSink`] / [`MockHost`] are deterministic test doubles; `agentero-web`
//! implements the trait over its WebSocket event bus.

use agentero_core::error::AppError;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A captured event, in emission order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedEvent {
    pub event: String,
    pub payload: Value,
    /// `Some(label)` = targeted (desktop `emit_to`); `None` = broadcast.
    pub target: Option<String>,
}

/// Event sink. Implementations must never block: ACP notification handlers call
/// this from async tasks, and a slow sink would stall the agent stream.
pub trait HostSink: Send + Sync {
    fn emit(&self, event: &str, payload: Value, target: Option<String>);
}

/// Drops every event (headless CLI / unit tests that do not assert events).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSink;

impl HostSink for NoopSink {
    fn emit(&self, _event: &str, _payload: Value, _target: Option<String>) {}
}

/// Records events in memory so tests can assert the exact event sequence.
#[derive(Debug, Default, Clone)]
pub struct MockSink {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl MockSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of every event emitted so far.
    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().expect("mock sink lock").clone()
    }

    /// Event names only — handy for `assert_eq!` on ordering.
    pub fn event_names(&self) -> Vec<String> {
        self.events().into_iter().map(|event| event.event).collect()
    }

    /// First event with the given name.
    pub fn find(&self, name: &str) -> Option<CapturedEvent> {
        self.events().into_iter().find(|event| event.event == name)
    }

    /// Drop recorded events (between test phases).
    pub fn clear(&self) {
        self.events.lock().expect("mock sink lock").clear();
    }
}

impl HostSink for MockSink {
    fn emit(&self, event: &str, payload: Value, target: Option<String>) {
        self.events
            .lock()
            .expect("mock sink lock")
            .push(CapturedEvent {
                event: event.to_string(),
                payload,
                target,
            });
    }
}

impl HostSink for Arc<dyn HostSink> {
    fn emit(&self, event: &str, payload: Value, target: Option<String>) {
        (**self).emit(event, payload, target);
    }
}

/// Everything needed to start one ACP agent process.
#[derive(Debug, Clone)]
pub struct AgentProcessSpec {
    /// Descriptor command (already resolved by the registry/launch policy).
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    /// ACP session cwd; the stdio transport has no cwd field, so hosts that
    /// support it may apply it natively, others prepend a shell wrapper.
    pub cwd: Option<PathBuf>,
}

/// Host-resolved launch plan (program + argv + env). The ACP SDK spawns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProcess {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Host services used by the agent domain.
///
/// Deliberately **not** a Tauri trait object: web implements it over axum/WS,
/// desktop can implement it over `AppHandle` in `src-tauri` (phase C).
pub trait Host: HostSink {
    /// Run a background task. Default: `tokio::spawn` (web/headless behavior).
    fn spawn(&self, task: BoxFuture<'static, ()>) {
        tokio::spawn(task);
    }

    /// Directory holding `agents.json` (and other per-user agent state).
    fn config_dir(&self) -> PathBuf;

    /// Directory holding bundled ACP adapters (`adapters/manifest.json`).
    fn resource_dir(&self) -> Option<PathBuf> {
        None
    }

    /// Same sink as `self`, as an owned trait object for background tasks
    /// (`AgentEventEmitter` must be `'static`).
    fn sink_arc(&self) -> Arc<dyn HostSink>;

    /// Resolve the launch plan for an ACP child process.
    ///
    /// Default is local `exec`: the command is looked up on the host `PATH` and
    /// launched with the given argv/env. A containerized web deployment can
    /// return a `docker exec` plan; a remote host an `ssh …` plan.
    fn spawn_agent_process(&self, spec: AgentProcessSpec) -> Result<AgentProcess, AppError> {
        let program = which::which(&spec.command).unwrap_or_else(|_| PathBuf::from(&spec.command));
        Ok(AgentProcess {
            program,
            args: spec.args,
            env: spec.env,
        })
    }
}

/// Default host for the headless server and tests.
#[derive(Clone)]
pub struct LocalHost {
    sink: Arc<dyn HostSink>,
    config_dir: PathBuf,
    resource_dir: Option<PathBuf>,
}

impl LocalHost {
    /// `config_dir` is explicit so tests never touch the user's real config.
    pub fn new(sink: Arc<dyn HostSink>, config_dir: impl Into<PathBuf>) -> Self {
        Self {
            sink,
            config_dir: config_dir.into(),
            resource_dir: std::env::var_os("AGENTERO_RESOURCE_DIR").map(PathBuf::from),
        }
    }

    /// Default per-user config dir (XDG `<config>/agentero` on Unix), matching
    /// the desktop `agents.json` location so a migrated instance keeps its
    /// registry.
    pub fn default_config_dir() -> PathBuf {
        agentero_core::paths::agentero_config_dir()
    }

    /// Convenience constructor for the server: default config dir + a sink.
    pub fn with_sink(sink: Arc<dyn HostSink>) -> Self {
        Self::new(sink, Self::default_config_dir())
    }

    pub fn with_resource_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.resource_dir = Some(dir.into());
        self
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_dir.join("agents.json")
    }
}

impl HostSink for LocalHost {
    fn emit(&self, event: &str, payload: Value, target: Option<String>) {
        self.sink.emit(event, payload, target);
    }
}

impl Host for LocalHost {
    fn config_dir(&self) -> PathBuf {
        self.config_dir.clone()
    }

    fn resource_dir(&self) -> Option<PathBuf> {
        self.resource_dir.clone()
    }

    fn sink_arc(&self) -> Arc<dyn HostSink> {
        self.sink.clone()
    }
}

/// Test host: mock sink + a temp config dir + in-process spawn.
pub struct MockHost {
    sink: MockSink,
    config_dir: PathBuf,
    resource_dir: Option<PathBuf>,
    /// Keep the temp dir alive for the host's lifetime.
    _temp: Option<tempfile::TempDir>,
}

impl MockHost {
    /// Host rooted at a fresh temp dir.
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp config dir");
        Self {
            sink: MockSink::new(),
            config_dir: temp.path().to_path_buf(),
            resource_dir: None,
            _temp: Some(temp),
        }
    }

    /// Host rooted at an explicit directory (caller owns cleanup).
    pub fn with_config_dir(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            sink: MockSink::new(),
            config_dir: config_dir.into(),
            resource_dir: None,
            _temp: None,
        }
    }

    pub fn with_resource_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.resource_dir = Some(dir.into());
        self
    }

    pub fn sink(&self) -> &MockSink {
        &self.sink
    }

    pub fn events(&self) -> Vec<CapturedEvent> {
        self.sink.events()
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_dir.join("agents.json")
    }
}

impl Default for MockHost {
    fn default() -> Self {
        Self::new()
    }
}

impl HostSink for MockHost {
    fn emit(&self, event: &str, payload: Value, target: Option<String>) {
        self.sink.emit(event, payload, target);
    }
}

impl Host for MockHost {
    fn config_dir(&self) -> PathBuf {
        self.config_dir.clone()
    }

    fn resource_dir(&self) -> Option<PathBuf> {
        self.resource_dir.clone()
    }

    fn sink_arc(&self) -> Arc<dyn HostSink> {
        Arc::new(self.sink.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_sink_records_targeted_and_broadcast_events() {
        let sink = MockSink::new();
        sink.emit(
            "agent:status",
            serde_json::json!({"phase": "starting"}),
            Some("tab-1".into()),
        );
        sink.emit("agent:registry-changed", serde_json::json!({}), None);
        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].target.as_deref(), Some("tab-1"));
        assert_eq!(events[1].target, None);
        assert_eq!(events[1].payload, serde_json::json!({}));
        assert_eq!(
            sink.event_names(),
            ["agent:status", "agent:registry-changed"]
        );
    }

    #[test]
    fn mock_host_owns_a_temp_config_dir_and_resolves_local_launch() {
        let host = MockHost::new();
        assert!(host.config_dir().is_dir());
        assert_eq!(host.config_path().file_name().unwrap(), "agents.json");
        let plan = host
            .spawn_agent_process(AgentProcessSpec {
                command: "sh".to_string(),
                args: vec!["-c".to_string(), "true".to_string()],
                env: HashMap::new(),
                cwd: None,
            })
            .expect("resolve sh");
        assert!(plan.program.is_absolute());
        assert_eq!(plan.args, ["-c", "true"]);
    }
}
