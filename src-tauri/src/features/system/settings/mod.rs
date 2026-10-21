//! Desktop shell for the shared settings store.
//!
//! The store, schema and domain accessors are tauri-free and live in
//! `agentero_core::features::system::settings` (shared with the headless web
//! host); this module only adds the `#[tauri::command]` surface.

pub use agentero_core::features::system::settings::*;

/// Tauri command shells for this feature.
pub mod commands;
