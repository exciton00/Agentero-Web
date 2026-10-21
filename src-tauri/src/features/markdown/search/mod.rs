//! Desktop shell for the shared core Vault search.
//!
//! The walk/ranking implementation lives in
//! `agentero_core::features::markdown::search` so the headless Web host and the
//! desktop Host return identical hits; this module only keeps the desktop
//! command shell path stable.

pub use agentero_core::features::markdown::search::*;

pub mod commands;
