//! WeKnora RAG integration (P1 backend).
//!
//! One merged Markdown document per catalog paper is synced into a WeKnora
//! knowledge base through the manual-Markdown API (in-place update, no PDF
//! upload). See `docs/backend/weknora.md` and the reviewed plan
//! (`weknora-integration-plan.md`).
//!
//! Module layout:
//! - [`settings`] — persisted `settings.weknora` shape + defaults/normalize;
//! - [`client`] — WeKnora REST client (auth, endpoint wrappers, error mapping);
//! - [`render`] — paper package → merged Markdown (renderer v1);
//! - [`state`] — per-vault sync state file under `$XDG_DATA_HOME/agentero`;
//! - [`sync`] — scan → render → diff → execute → tags → index-wait →
//!   delete/reconcile engine.

pub mod client;
pub mod render;
pub mod settings;
pub mod state;
pub mod sync;

pub use settings::WeKnoraSettings;

/// Renderer version. Bump when the merged-Markdown layout changes: it is part
/// of every content hash, so a bump triggers a full re-sync of every paper.
pub const RENDERER_VERSION: u32 = 1;

/// Hard cap for a rendered document (WeKnora rejects/times out on huge docs).
/// Content above this is truncated with an explicit marker in the markdown.
pub const MAX_DOCUMENT_BYTES: usize = 300 * 1024;
