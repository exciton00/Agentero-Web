//! `settings.rag` — persisted RAG configuration (provider-agnostic).
//!
//! Lives in the shared `settings.json` (`AppSettings.rag`; legacy files use the
//! `weknora` key, accepted through a serde alias), so the desktop Host and the
//! headless Web host read the same configuration. The API key is a server-side
//! secret: `redact_secrets` masks it on the wire and `merge_secrets` restores
//! the stored value when the UI writes the mask back.

use serde::{Deserialize, Serialize};

use super::providers::RagProviderId;

/// Auto-sync fallback scan interval when unset (minutes).
pub const DEFAULT_AUTO_SYNC_MINUTES: u32 = 30;
/// Retrieval HTTP timeout when unset (milliseconds).
pub const DEFAULT_RETRIEVAL_TIMEOUT_MS: u64 = 15_000;
/// Default `topK` for retrieval.
pub const DEFAULT_TOP_K: u32 = 8;
/// Default sync worker concurrency.
pub const DEFAULT_CONCURRENCY: u32 = 2;
/// Default per-document index wait budget (seconds).
pub const DEFAULT_INDEX_TIMEOUT_SECS: u64 = 180;

/// Which parts of a paper package are merged into the synced document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct RagSyncSettings {
    /// `PAPER.md` body (metadata-only docs are allowed when missing).
    #[serde(default = "default_true")]
    pub include_paper_body: bool,
    /// `NOTES.md` user notes (no frontmatter, images become text placeholders).
    #[serde(default = "default_true")]
    pub include_notes: bool,
    /// `marks/annotations.json` + `marks/*.json` text (quote/comment).
    #[serde(default = "default_true")]
    pub include_marks: bool,
    /// `source/layout-index.json` figure/table index (falls back to PAPER.md).
    #[serde(default = "default_true")]
    pub include_figure_index: bool,
    /// BibTeX key / `\cite{}` / bibtex+biblatex entries.
    #[serde(default = "default_true")]
    pub include_citation: bool,
    /// Queue body parsing first when a paper has a PDF but no `PAPER.md`.
    #[serde(default = "default_true")]
    pub ensure_paper_body: bool,
    /// Auto-sync after Vault writes (60s debounce) + periodic fallback scan.
    #[serde(default)]
    pub auto_sync: bool,
    /// Periodic fallback scan interval; also the debounce cap. Default 30 min.
    #[serde(default = "default_auto_sync_minutes")]
    pub auto_sync_minutes: u32,
}

impl Default for RagSyncSettings {
    fn default() -> Self {
        Self {
            include_paper_body: true,
            include_notes: true,
            include_marks: true,
            include_figure_index: true,
            include_citation: true,
            ensure_paper_body: true,
            auto_sync: false,
            auto_sync_minutes: DEFAULT_AUTO_SYNC_MINUTES,
        }
    }
}

/// Which agent surfaces may use the read-only RAG tools (P2 gates on these).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct RagToolSettings {
    #[serde(default = "default_true")]
    pub expose_to_agent: bool,
    #[serde(default = "default_true")]
    pub expose_to_mcp: bool,
}

impl Default for RagToolSettings {
    fn default() -> Self {
        Self {
            expose_to_agent: true,
            expose_to_mcp: true,
        }
    }
}

/// Retrieval defaults (shared by the settings UI and the future agent tools).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct RagRetrievalSettings {
    /// Rerank is off by default: the tested instance can stall 25–40s with it.
    #[serde(default)]
    pub rerank: bool,
    /// Search request timeout (ms). On timeout the service degrades to an
    /// un-reranked retry when re-rank was requested.
    #[serde(default = "default_retrieval_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
}

impl Default for RagRetrievalSettings {
    fn default() -> Self {
        Self {
            rerank: false,
            timeout_ms: DEFAULT_RETRIEVAL_TIMEOUT_MS,
            top_k: DEFAULT_TOP_K,
        }
    }
}

/// Sync-engine tuning knobs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct RagAdvancedSettings {
    /// Concurrent document writes (create/update). Clamped to 1..=4.
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
    /// Per-document index wait budget; on timeout the doc is marked `indexing`
    /// and left for reconcile instead of blocking the run.
    #[serde(default = "default_index_timeout_secs")]
    pub index_timeout_secs: u64,
}

impl Default for RagAdvancedSettings {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_CONCURRENCY,
            index_timeout_secs: DEFAULT_INDEX_TIMEOUT_SECS,
        }
    }
}

/// RAG configuration (`settings.rag`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct RagSettings {
    /// RAG backend. `weknora` (implemented) or `ragflow` (reserved, returns
    /// `not_implemented` until an adapter lands).
    #[serde(default)]
    pub provider: RagProviderId,
    /// Explicit opt-in: notes/annotations text leaves the Vault when on.
    #[serde(default)]
    pub enabled: bool,
    /// e.g. `https://weknora.example.com` or `…/api/v1`; normalized by the client.
    #[serde(default)]
    pub base_url: String,
    /// Server-side secret; masked on read.
    #[serde(default)]
    pub api_key: String,
    /// Target knowledge base id.
    #[serde(default)]
    pub knowledge_base_id: String,
    #[serde(default)]
    pub sync: RagSyncSettings,
    #[serde(default)]
    pub tools: RagToolSettings,
    #[serde(default)]
    pub retrieval: RagRetrievalSettings,
    #[serde(default)]
    pub advanced: RagAdvancedSettings,
}

impl RagSettings {
    /// Configured provider id.
    pub fn provider(&self) -> RagProviderId {
        self.provider
    }

    /// Base URL + knowledge base id are present (the key may still be missing
    /// for self-hosted instances that run anonymous — the client decides).
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.knowledge_base_id.trim().is_empty()
    }

    /// Trimmed base URL (empty when unset).
    pub fn base_url(&self) -> &str {
        self.base_url.trim()
    }

    /// Trimmed knowledge base id (empty when unset).
    pub fn knowledge_base_id(&self) -> &str {
        self.knowledge_base_id.trim()
    }

    /// Stored API key; `None` when empty or still a redaction mask.
    pub fn api_key(&self) -> Option<&str> {
        let key = self.api_key.trim();
        if key.is_empty() || crate::features::system::settings::is_translate_api_key_mask(key) {
            None
        } else {
            Some(key)
        }
    }

    /// Trim/clamp mutable fields (called by `AppSettings` normalize on load and
    /// save, so a hand-edited settings.json cannot persist nonsense).
    pub fn normalize(&mut self) {
        self.base_url = normalize_base_url_value(&self.base_url);
        self.api_key = self.api_key.trim().to_string();
        self.knowledge_base_id = self.knowledge_base_id.trim().to_string();
        self.sync.auto_sync_minutes = self.sync.auto_sync_minutes.clamp(1, 24 * 60);
        self.retrieval.timeout_ms = self.retrieval.timeout_ms.clamp(1_000, 120_000);
        self.retrieval.top_k = self.retrieval.top_k.clamp(1, 50);
        self.advanced.concurrency = self.advanced.concurrency.clamp(1, 4);
        self.advanced.index_timeout_secs = self.advanced.index_timeout_secs.clamp(10, 3_600);
    }
}

/// Normalize a user-entered base URL: trim, drop trailing slashes and keep a
/// possible `/api/v1` suffix. Invalid values are left trimmed (the client
/// reports a readable validation error instead of silently rewriting them).
pub fn normalize_base_url_value(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

fn default_true() -> bool {
    true
}
fn default_auto_sync_minutes() -> u32 {
    DEFAULT_AUTO_SYNC_MINUTES
}
fn default_retrieval_timeout_ms() -> u64 {
    DEFAULT_RETRIEVAL_TIMEOUT_MS
}
fn default_top_k() -> u32 {
    DEFAULT_TOP_K
}
fn default_concurrency() -> u32 {
    DEFAULT_CONCURRENCY
}
fn default_index_timeout_secs() -> u64 {
    DEFAULT_INDEX_TIMEOUT_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_reviewed_contract() {
        let settings = RagSettings::default();
        assert!(!settings.enabled);
        assert!(!settings.is_configured());
        assert!(settings.sync.include_paper_body);
        assert!(settings.sync.include_notes);
        assert!(settings.sync.include_marks);
        assert!(settings.sync.include_figure_index);
        assert!(settings.sync.include_citation);
        assert!(settings.sync.ensure_paper_body);
        assert!(!settings.sync.auto_sync);
        assert_eq!(settings.sync.auto_sync_minutes, 30);
        assert!(settings.tools.expose_to_agent);
        assert!(settings.tools.expose_to_mcp);
        assert!(!settings.retrieval.rerank);
        assert_eq!(settings.retrieval.timeout_ms, 15_000);
        assert_eq!(settings.retrieval.top_k, 8);
        assert_eq!(settings.advanced.concurrency, 2);
        assert_eq!(settings.advanced.index_timeout_secs, 180);
    }

    #[test]
    fn old_settings_without_weknora_default_and_partial_json_keeps_defaults() {
        let from_json: RagSettings = serde_json::from_str("{}").expect("empty object");
        assert_eq!(from_json, RagSettings::default());
        let partial: RagSettings =
            serde_json::from_str(r#"{"enabled":true,"sync":{"autoSync":true}}"#)
                .expect("partial");
        assert!(partial.enabled);
        assert!(partial.sync.auto_sync);
        // Omitted nested booleans fall back to their documented defaults.
        assert!(partial.sync.include_notes);
        assert_eq!(partial.sync.auto_sync_minutes, 30);
    }

    #[test]
    fn normalize_trims_and_clamps() {
        let mut settings = RagSettings {
            base_url: "  https://kb.example.com/api/v1/  ".into(),
            api_key: "  sk-test  ".into(),
            knowledge_base_id: "  kb-1  ".into(),
            sync: RagSyncSettings {
                auto_sync_minutes: 0,
                ..Default::default()
            },
            retrieval: RagRetrievalSettings {
                timeout_ms: 10,
                top_k: 0,
                ..Default::default()
            },
            advanced: RagAdvancedSettings {
                concurrency: 0,
                index_timeout_secs: 5_000,
                ..Default::default()
            },
            ..Default::default()
        };
        settings.normalize();
        assert_eq!(settings.base_url, "https://kb.example.com/api/v1");
        assert_eq!(settings.api_key, "sk-test");
        assert_eq!(settings.knowledge_base_id, "kb-1");
        assert_eq!(settings.sync.auto_sync_minutes, 1);
        assert_eq!(settings.retrieval.timeout_ms, 1_000);
        assert_eq!(settings.retrieval.top_k, 1);
        assert_eq!(settings.advanced.concurrency, 1);
        assert_eq!(settings.advanced.index_timeout_secs, 3_600);
        assert!(settings.is_configured());
        assert_eq!(settings.api_key(), Some("sk-test"));
    }

    #[test]
    fn masked_or_empty_api_key_is_treated_as_unset() {
        let mut settings = RagSettings {
            api_key: "*********".into(),
            ..Default::default()
        };
        assert_eq!(settings.api_key(), None);
        settings.api_key = String::new();
        assert_eq!(settings.api_key(), None);
        settings.api_key = "real-key".into();
        assert_eq!(settings.api_key(), Some("real-key"));
    }

    #[test]
    fn serialized_shape_is_camel_case() {
        let value = serde_json::to_value(RagSettings::default()).unwrap();
        for key in [
            "enabled",
            "baseUrl",
            "apiKey",
            "knowledgeBaseId",
            "sync",
            "tools",
            "retrieval",
            "advanced",
        ] {
            assert!(value.get(key).is_some(), "missing {key}: {value}");
        }
        assert!(value["sync"].get("includePaperBody").is_some());
        assert!(value["sync"].get("autoSyncMinutes").is_some());
        assert!(value["retrieval"].get("timeoutMs").is_some());
        assert!(value["advanced"].get("indexTimeoutSecs").is_some());
    }
}
