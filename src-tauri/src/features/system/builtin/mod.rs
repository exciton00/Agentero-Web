//! Desktop command shell for the shared built-in provider descriptors.
//!
//! Credential injection and descriptor fallbacks live in
//! `agentero_core::features::system::builtin` (shared with the headless web
//! host); only the `#[tauri::command]` surface stays here.

pub use agentero_core::features::system::builtin::*;

#[tauri::command]
#[specta::specta]
pub fn builtin_provider_status() -> crate::core::error::ApiResult<BuiltinProviderStatus> {
    crate::core::error::ApiResult::ok(status())
}
