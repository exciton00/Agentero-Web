//! Per-Vault, per-provider RAG sync state.
//!
//! Stored **outside the Vault** (never committed, never synced):
//! `$XDG_DATA_HOME/agentero/rag/<sha256(vault path)[:16]>.<provider>.json`.
//! Legacy (v2.0) installs wrote `$XDG_DATA_HOME/agentero/weknora/<vault>.json`;
//! [`RagSyncState::load`] reads that file once when the provider is WeKnora and
//! the new path is absent, then the next save writes the new location.
//!
//! The file never contains the API key; it only maps paper paths to provider
//! knowledge ids plus content hashes and index status.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::features::rag::providers::RagProviderId;
use crate::features::rag::RENDERER_VERSION;
use crate::paths::agentero_data_dir;

pub const STATE_SCHEMA_VERSION: u32 = 1;

/// `ready` | `indexing` | `failed`.
pub const STATUS_READY: &str = "ready";
pub const STATUS_INDEXING: &str = "indexing";
pub const STATUS_FAILED: &str = "failed";

/// One paper → provider document mapping.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RagDocState {
    pub knowledge_id: String,
    /// `sha256(renderer version + render options + markdown)`.
    pub content_hash: String,
    #[serde(default)]
    pub bibtex_key: String,
    #[serde(default)]
    pub tag_ids: Vec<String>,
    /// `ready` | `indexing` | `failed`.
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub synced_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl RagDocState {
    pub fn is_ready(&self) -> bool {
        self.status == STATUS_READY
    }
}

fn default_status() -> String {
    STATUS_READY.to_string()
}

/// Whole-Vault sync state file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RagSyncState {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Provider this mapping belongs to (v2.0 files have no field: WeKnora).
    #[serde(default)]
    pub provider: RagProviderId,
    /// Knowledge base this mapping belongs to. A KB change discards the mapping
    /// (documents live in another KB and must be re-created there).
    #[serde(default)]
    pub kb_id: String,
    #[serde(default = "default_renderer_version")]
    pub renderer_version: u32,
    #[serde(default)]
    pub last_sync_at: Option<String>,
    /// Vault-relative paper path → document mapping.
    #[serde(default)]
    pub docs: BTreeMap<String, RagDocState>,
}

fn default_schema_version() -> u32 {
    STATE_SCHEMA_VERSION
}
fn default_renderer_version() -> u32 {
    RENDERER_VERSION
}

impl Default for RagSyncState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            provider: RagProviderId::WeKnora,
            kb_id: String::new(),
            renderer_version: RENDERER_VERSION,
            last_sync_at: None,
            docs: BTreeMap::new(),
        }
    }
}

impl RagSyncState {
    /// Pin the mapping to `provider`; a provider switch resets the mapping.
    pub fn prepare_for_provider(&mut self, provider: RagProviderId) -> bool {
        if self.provider == provider {
            return false;
        }
        self.provider = provider;
        self.docs.clear();
        self.last_sync_at = None;
        self.kb_id.clear();
        true
    }

    /// Point the mapping at `kb_id`; returns true when the mapping was reset
    /// because the knowledge base changed (all documents must be re-created).
    pub fn prepare_for_kb(&mut self, kb_id: &str) -> bool {
        self.schema_version = STATE_SCHEMA_VERSION;
        self.renderer_version = RENDERER_VERSION;
        if self.kb_id == kb_id {
            return false;
        }
        self.kb_id = kb_id.to_string();
        self.docs.clear();
        self.last_sync_at = None;
        true
    }

    /// Load the state for `vault`/`provider`.
    ///
    /// Falls back to the legacy `agentero/weknora/<vault>.json` file (v2.0) when
    /// the provider-aware path does not exist yet; the caller's next `save`
    /// migrates it. A missing/corrupt file yields defaults (reconcile can rebuild
    /// the mapping from provider custom_metadata).
    pub fn load(vault: &Path, provider: RagProviderId) -> Self {
        let path = state_path(vault, provider);
        let legacy = legacy_state_path(vault);
        let (read_path, from_legacy) = if path.is_file() {
            (path, false)
        } else if provider == RagProviderId::WeKnora && legacy.is_file() {
            (legacy, true)
        } else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&read_path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut state) => {
                state.provider = provider;
                if from_legacy {
                    log::info!(
                        "rag: migrated legacy sync state {} to provider-aware storage",
                        read_path.display()
                    );
                }
                state
            }
            Err(error) => {
                log::warn!(
                    "rag: ignoring unreadable sync state {}: {error}",
                    read_path.display()
                );
                Self::default()
            }
        }
    }

    /// Persist atomically (the sync engine saves after every document).
    pub fn save(&self, vault: &Path, provider: RagProviderId) -> Result<(), AppError> {
        let path = state_path(vault, provider);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                AppError::message(format!(
                    "cannot create RAG state dir {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| AppError::message(format!("serialize RAG state: {error}")))?;
        crate::fs::atomic_write(&path, &bytes).map_err(|error| {
            AppError::message(format!("cannot write RAG state {}: {error}", path.display()))
        })
    }

    /// Status counters used by `rag_status`.
    pub fn counts(&self) -> RagStateCounts {
        let mut counts = RagStateCounts::default();
        for doc in self.docs.values() {
            match doc.status.as_str() {
                STATUS_READY => counts.synced += 1,
                STATUS_INDEXING => counts.indexing += 1,
                STATUS_FAILED => counts.failed += 1,
                _ => counts.failed += 1,
            }
        }
        counts.total = self.docs.len();
        counts
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RagStateCounts {
    pub total: usize,
    pub synced: usize,
    pub indexing: usize,
    pub failed: usize,
}

/// Stable 16-hex vault key: `sha256(absolute vault path)[:16]`.
pub fn vault_key(vault: &Path) -> String {
    let canonical = crate::fs::canonicalize_best_effort(vault);
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    hex::encode(digest)[..16].to_string()
}

/// Absolute provider-aware state-file path for a Vault.
pub fn state_path(vault: &Path, provider: RagProviderId) -> PathBuf {
    agentero_data_dir()
        .join("rag")
        .join(format!("{}.{}.json", vault_key(vault), provider.id()))
}

/// v2.0 state path (WeKnora-only installs). Read-only fallback.
pub fn legacy_state_path(vault: &Path) -> PathBuf {
    agentero_data_dir()
        .join("weknora")
        .join(format!("{}.json", vault_key(vault)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault_fixture() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn sample_state() -> RagSyncState {
        let mut state = RagSyncState::default();
        assert!(state.prepare_for_kb("kb-1"));
        state.docs.insert(
            "papers/a".into(),
            RagDocState {
                knowledge_id: "doc-1".into(),
                content_hash: "hash-1".into(),
                bibtex_key: "vaswani2017".into(),
                tag_ids: vec!["tag-1".into()],
                status: STATUS_READY.into(),
                synced_at: Some("2026-10-24T00:00:00.000Z".into()),
                last_error: None,
            },
        );
        state.last_sync_at = Some("2026-10-24T00:00:00.000Z".into());
        state
    }

    #[test]
    fn state_path_is_provider_scoped_and_outside_the_vault() {
        let vault = vault_fixture();
        let weknora = state_path(vault.path(), RagProviderId::WeKnora);
        let ragflow = state_path(vault.path(), RagProviderId::RagFlow);
        assert_eq!(weknora, state_path(vault.path(), RagProviderId::WeKnora));
        assert_ne!(weknora, ragflow);
        assert!(weknora.to_string_lossy().contains("agentero/rag"));
        assert!(weknora.to_string_lossy().ends_with(".weknora.json"));
        assert!(ragflow.to_string_lossy().ends_with(".ragflow.json"));
        assert!(
            !weknora.starts_with(vault.path()),
            "state must not live inside the vault"
        );
        assert_eq!(
            weknora.file_stem().unwrap().to_string_lossy().len(),
            format!("{}.weknora", vault_key(vault.path())).len()
        );
    }

    #[test]
    fn save_and_load_roundtrip_never_contains_secrets() {
        let vault = vault_fixture();
        let state = sample_state();
        state
            .save(vault.path(), RagProviderId::WeKnora)
            .expect("save");
        let loaded = RagSyncState::load(vault.path(), RagProviderId::WeKnora);
        assert_eq!(loaded, state);
        assert_eq!(loaded.counts().synced, 1);
        assert_eq!(loaded.counts().total, 1);

        let path = state_path(vault.path(), RagProviderId::WeKnora);
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(!raw.contains("apiKey"), "{raw}");
        assert!(!raw.contains("api_key"), "{raw}");
        let _ = std::fs::remove_file(state_path(vault.path(), RagProviderId::WeKnora));
    }

    #[test]
    fn legacy_weknora_state_is_read_once_then_migrated() {
        let vault = vault_fixture();
        let legacy = legacy_state_path(vault.path());
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        // v2.0 file: no provider field, top-level layout lacked it.
        std::fs::write(&legacy, serde_json::to_vec(&sample_state()).unwrap()).unwrap();
        let loaded = RagSyncState::load(vault.path(), RagProviderId::WeKnora);
        assert_eq!(loaded.counts().synced, 1);
        assert_eq!(loaded.provider, RagProviderId::WeKnora);
        // Next save writes the new provider-aware path.
        loaded
            .save(vault.path(), RagProviderId::WeKnora)
            .expect("save");
        assert!(state_path(vault.path(), RagProviderId::WeKnora).is_file());
        let _ = std::fs::remove_file(legacy);
        let _ = std::fs::remove_file(state_path(vault.path(), RagProviderId::WeKnora));
    }

    #[test]
    fn legacy_state_is_not_used_for_other_providers() {
        let vault = vault_fixture();
        let legacy = legacy_state_path(vault.path());
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, serde_json::to_vec(&sample_state()).unwrap()).unwrap();
        let loaded = RagSyncState::load(vault.path(), RagProviderId::RagFlow);
        assert!(loaded.docs.is_empty());
        let _ = std::fs::remove_file(legacy);
    }

    #[test]
    fn kb_change_resets_the_mapping() {
        let mut state = RagSyncState::default();
        state.prepare_for_kb("kb-1");
        state.docs.insert(
            "papers/a".into(),
            RagDocState {
                knowledge_id: "doc-1".into(),
                content_hash: "h".into(),
                bibtex_key: String::new(),
                tag_ids: Vec::new(),
                status: STATUS_READY.into(),
                synced_at: None,
                last_error: None,
            },
        );
        assert!(!state.prepare_for_kb("kb-1"), "same KB keeps the mapping");
        assert!(state.prepare_for_kb("kb-2"), "different KB resets");
        assert!(state.docs.is_empty());
        assert_eq!(state.kb_id, "kb-2");
    }

    #[test]
    fn provider_switch_resets_the_mapping() {
        let mut state = sample_state();
        assert!(!state.prepare_for_provider(RagProviderId::WeKnora));
        assert!(state.prepare_for_provider(RagProviderId::RagFlow));
        assert!(state.docs.is_empty());
        assert_eq!(state.provider, RagProviderId::RagFlow);
    }

    #[test]
    fn corrupt_state_falls_back_to_defaults() {
        let vault = vault_fixture();
        let path = state_path(vault.path(), RagProviderId::WeKnora);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let state = RagSyncState::load(vault.path(), RagProviderId::WeKnora);
        assert!(state.docs.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn counts_classify_every_document() {
        let mut state = RagSyncState::default();
        for (path, status) in [
            ("papers/a", STATUS_READY),
            ("papers/b", STATUS_INDEXING),
            ("papers/c", STATUS_FAILED),
            ("papers/d", "unexpected"),
        ] {
            state.docs.insert(
                path.to_string(),
                RagDocState {
                    knowledge_id: path.to_string(),
                    content_hash: String::new(),
                    bibtex_key: String::new(),
                    tag_ids: Vec::new(),
                    status: status.to_string(),
                    synced_at: None,
                    last_error: None,
                },
            );
        }
        let counts = state.counts();
        assert_eq!(counts.total, 4);
        assert_eq!(counts.synced, 1);
        assert_eq!(counts.indexing, 1);
        assert_eq!(counts.failed, 2);
    }
}
