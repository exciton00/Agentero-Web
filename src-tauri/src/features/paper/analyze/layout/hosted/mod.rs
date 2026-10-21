//! Desktop shell for the shared remote layout-analysis engines.
//!
//! The engines and wire types live in
//! `agentero_core::features::paper::analyze::layout_hosted` (single source of
//! truth for the desktop Host and the headless Web host); this module only
//! keeps the historical desktop path stable and hosts the `#[tauri::command]`
//! shells.

pub use agentero_core::features::paper::analyze::layout_hosted::{
    engine, mineru, openai_compatible, paddle,
};
pub use agentero_core::features::paper::analyze::layout_hosted::*;

pub mod commands;
