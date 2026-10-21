//! Process-wide state shared by every request.

use std::sync::Arc;

use agentero_core::features::paper::capabilities::CapsCache;
use agentero_core::features::system::settings::AppSettingsStore;

use crate::config::Config;
use crate::events::{EventBus, WebHostHooks};

pub struct AppState {
    pub config: Config,
    /// Durable XDG `settings.json`, shared schema with the desktop Host.
    pub settings: Arc<AppSettingsStore>,
    pub events: EventBus,
    /// Per-paper capability cache (PDF/TeX/PAPER.md probes).
    pub caps: CapsCache,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let settings = Arc::new(AppSettingsStore::load());
        Self {
            config,
            settings,
            events: EventBus::new(),
            caps: CapsCache::new(),
        }
    }

    /// Core host handle wired to the web event bus (used by migrated domain
    /// services such as import / parse, whose emit + follow-up job callbacks
    /// are inverted through `HostHooks`).
    pub fn host_handle(&self) -> agentero_core::app_handle::AppHandle {
        agentero_core::app_handle::AppHandle::with_hooks(Arc::new(WebHostHooks {
            bus: self.events.clone(),
        }))
    }
}
