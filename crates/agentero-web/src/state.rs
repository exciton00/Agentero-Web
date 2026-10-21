//! Process-wide state shared by every request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agentero_core::features::paper::capabilities::CapsCache;
use agentero_core::features::system::settings::AppSettingsStore;

use crate::config::Config;
use crate::events::{EventBus, WebHostHooks};
use crate::handlers::runtime::sync::SyncService;
use crate::tasks::TaskCenter;
use crate::watcher::FsWatchService;

pub struct AppState {
    pub config: Config,
    /// Durable XDG `settings.json`, shared schema with the desktop Host.
    pub settings: Arc<AppSettingsStore>,
    pub events: EventBus,
    /// Per-paper capability cache (PDF/TeX/PAPER.md probes).
    pub caps: CapsCache,
    /// Multi-tab Vault filesystem watcher (`fs_watch_start` / `fs_watch_stop`).
    pub watcher: FsWatchService,
    /// Server-side task center (`job_*` commands + `job:*` events).
    pub tasks: TaskCenter,
    /// Per-vault cloud-sync run lease (`sync_*` commands + `sync:*` events).
    pub sync: Arc<SyncService>,
    /// Live browser-tab connections keyed by window label (targeted events).
    connections: Mutex<HashMap<String, usize>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let settings = Arc::new(AppSettingsStore::load());
        let events = EventBus::new();
        let caps = CapsCache::new();
        let watcher = FsWatchService::new(events.clone(), caps.clone());
        let tasks = TaskCenter::new(events.clone());
        let sync = Arc::new(SyncService::new(events.clone()));
        Self {
            config,
            settings,
            events,
            caps,
            watcher,
            tasks,
            sync,
            connections: Mutex::new(HashMap::new()),
        }
    }

    /// Register a WS connection for `label`; returns the connection count.
    pub fn connection_open(&self, label: &str) -> usize {
        let mut connections = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        let count = connections.entry(label.to_string()).or_insert(0);
        *count += 1;
        *count
    }

    /// Drop a WS connection; returns the remaining count for `label`.
    pub fn connection_close(&self, label: &str) -> usize {
        let mut connections = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        match connections.get_mut(label) {
            Some(count) => {
                *count = count.saturating_sub(1);
                let remaining = *count;
                if remaining == 0 {
                    connections.remove(label);
                }
                remaining
            }
            None => 0,
        }
    }

    /// Number of live connections for `label` (used by the close sweep).
    pub fn connection_count(&self, label: &str) -> usize {
        self.connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(label)
            .copied()
            .unwrap_or(0)
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
