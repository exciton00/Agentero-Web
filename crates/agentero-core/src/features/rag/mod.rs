//! RAG integration with pluggable providers.
//!
//! One merged Markdown document per catalog paper is synced into a knowledge
//! base (WeKnora today; RAGFlow reserved) through the manual-Markdown API
//! (in-place update, no PDF upload). See `docs/backend/rag.md` and the reviewed
//! plan (`weknora-integration-plan.md` §9).
//!
//! Module layout:
//! - [`settings`] — persisted `settings.rag` shape + provider + defaults/normalize;
//! - [`providers`] — provider ids/capabilities/backend dispatch + adapters;
//! - [`render`] — paper package → merged Markdown (renderer v1);
//! - [`state`] — per-vault/per-provider sync state under `$XDG_DATA_HOME/agentero/rag`;
//! - [`sync`] — scan → render → diff → execute → tags → index-wait →
//!   delete/reconcile engine.

pub mod providers;
pub mod render;
pub mod settings;
pub mod state;
pub mod sync;

pub use providers::{RagBackend, RagCapabilities, RagProviderId};
pub use settings::RagSettings;

/// Renderer version. Bump when the merged-Markdown layout changes: it is part
/// of every content hash, so a bump triggers a full re-sync of every paper.
pub const RENDERER_VERSION: u32 = 2;

/// Hard cap for a rendered document (providers reject/time out on huge docs).
/// Content above this is truncated with an explicit marker in the markdown.
pub const MAX_DOCUMENT_BYTES: usize = 300 * 1024;
