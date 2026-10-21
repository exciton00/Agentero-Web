//! Process-wide wiki state for the headless host.
//!
//! The desktop Host manages one [`WikiIndexState`] and one external-rename
//! repair store through Tauri state. The Web host has the same lifetime (one
//! process, single user), so both live here as lazily initialized statics: the
//! index serializes access through its own `Mutex`, which also gives the
//! multi-tab consistency the desktop relies on. If the platform layer later
//! moves these into `AppState`, handlers switch over without touching their
//! command bodies.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use agentero_core::features::vault::rename::{
    ExternalRenameRepairStore, WikiIndex, WikiIndexState,
};

static WIKI_INDEX: OnceLock<WikiIndexState> = OnceLock::new();
static REPAIR_STORE: OnceLock<ExternalRenameRepairStore> = OnceLock::new();

/// Cloneable index handle (same shape as `WikiIndexState::handle`).
pub fn wiki_index() -> Arc<Mutex<WikiIndex>> {
    WIKI_INDEX.get_or_init(WikiIndexState::new).handle()
}

/// Lock the shared index for one blocking operation.
pub fn lock_wiki_index() -> Result<MutexGuard<'static, WikiIndex>, String> {
    let state = WIKI_INDEX.get_or_init(WikiIndexState::new);
    state
        .inner
        .lock()
        .map_err(|error| format!("wiki index lock: {error}"))
}

/// Short-lived snapshots backing the external-rename preview/apply pair.
pub fn repair_store() -> &'static ExternalRenameRepairStore {
    REPAIR_STORE.get_or_init(ExternalRenameRepairStore::new)
}
