//! System-level application settings shared by every host.
//!
//! `AppSettings` is the durable XDG `settings.json` schema; the store is
//! tauri-free so the desktop Host, the headless web host and tooling all read
//! and write the same file. Host-specific command shells live in their own
//! crates.

pub mod builtin;
pub mod settings;
