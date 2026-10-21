//! Tauri-free host for the Agentero BYOA/ACP agent domain.
//!
//! The desktop implementation lives in `src-tauri/src/features/agent`; this
//! crate is the same domain with the three host seams (event emit, background
//! spawn, host paths/launch) abstracted behind [`host::HostSink`] /
//! [`host::Host`]. See `docs/development/webui-agent-port.md` for the
//! module-by-module migration table.
//!
//! Nothing here may depend on `tauri` / `wry` / `tao`; `agentero-web` consumes
//! this crate and implements its sink over the WebSocket event bus.

// Compatibility aliases: the moved sources keep their desktop import paths.
// `crate::core::X` resolves to the shared tauri-free foundations.
pub use agentero_core as core;

pub mod acp;
pub mod citation;
pub mod context_turn;
pub mod doctor;
pub mod doctor_agents;
pub mod host;
pub mod models;
pub mod prompt;
pub mod registry;
pub mod remote_host;
pub mod runtime;
pub mod service;
pub mod session;

// The ported sources still say `crate::features::agent::<module>`; these
// re-exports keep them byte-identical and make future upstream cherry-picks
// cheap. They are implementation details, not part of the public API.
#[doc(hidden)]
pub mod features {
    pub mod agent {
        pub use crate::acp;
        pub use crate::citation;
        pub use crate::context_turn;
        pub use crate::doctor;
        pub use crate::doctor_agents;
        pub use crate::models;
        pub use crate::prompt;
        pub use crate::registry;
        pub use crate::remote_host;
        pub use crate::runtime;
        pub use crate::session;
    }
}

pub use acp::{probe_agent, PermissionPolicy};
pub use citation::{resolve_citation, CitationTarget};
pub use doctor_agents::diagnose_agents;
pub use host::{Host, HostSink, LocalHost, MockHost, MockSink, NoopSink};
pub use models::AgentTelemetrySummary;
pub use registry::{builtin_templates, catalog_templates, AgentRegistry};
pub use runtime::{
    AgentEventEmitter, AgentRunController, AgentWarmGate, AskUserGate, ElicitationGate,
    PermissionGate,
};
pub use service::AgentHost;
pub use session::{
    list_acp_sessions, load_acp_session, new_ids, run_once, warm_agent, AgentWarmPool,
    RunOnceParams,
};
