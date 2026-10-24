//! RAG provider abstraction.
//!
//! WeKnora is the first implemented backend; RAGFlow is reserved and returns a
//! readable "not implemented" error until an adapter exists. Add a provider by:
//! 1. adding a [`RagProviderId`] variant (id/label/capabilities),
//! 2. adding a [`RagBackend`] variant + HTTP adapter module (model:
//!    [`weknora`]),
//! 3. implementing the [`super::sync::KnowledgeApi`] dispatch arm in
//!    `sync.rs` and mapping shared DTOs.

pub mod weknora;

use serde::{Deserialize, Serialize};

use super::settings::RagSettings;

pub use weknora::{
    MAX_CUSTOM_METADATA_FIELDS, MAX_CUSTOM_METADATA_VALUE_CHARS, KnowledgeBaseInfo, KnowledgeInfo, SearchHit, SearchMode, SearchRequest, TagInfo, WeKnoraClient,
    WeKnoraError, WeKnoraErrorKind,
};

/// Stable provider id persisted in settings/state/metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum RagProviderId {
    #[default]
    WeKnora,
    /// Reserved: no adapter yet (RPC/tools return `not_implemented`).
    RagFlow,
}

impl RagProviderId {
    pub fn id(self) -> &'static str {
        match self {
            Self::WeKnora => "weknora",
            Self::RagFlow => "ragflow",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::WeKnora => "WeKnora",
            Self::RagFlow => "RAGFlow",
        }
    }

    pub fn is_implemented(self) -> bool {
        matches!(self, Self::WeKnora)
    }

    pub fn capabilities(self) -> RagCapabilities {
        match self {
            Self::WeKnora => RagCapabilities {
                manual_markdown: true,
                in_place_update: true,
                tags: true,
                custom_metadata: true,
            },
            Self::RagFlow => RagCapabilities {
                manual_markdown: true,
                in_place_update: false,
                tags: true,
                custom_metadata: false,
            },
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "weknora" => Some(Self::WeKnora),
            "ragflow" => Some(Self::RagFlow),
            _ => None,
        }
    }
}

impl std::fmt::Display for RagProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// What a backend can do. `sync` uses this to pick manual-Markdown in-place
/// updates vs. delete+create, and whether tags/custom metadata are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RagCapabilities {
    pub manual_markdown: bool,
    pub in_place_update: bool,
    pub tags: bool,
    pub custom_metadata: bool,
}

/// Shared knowledge-base summary (provider-tagged) for RPC/UI listings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbSummary {
    pub provider: RagProviderId,
    pub provider_label: &'static str,
    pub id: String,
    pub name: String,
    pub knowledge_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<serde_json::Value>,
}

impl KbSummary {
    pub fn from_weknora(provider: RagProviderId, info: KnowledgeBaseInfo) -> Self {
        Self {
            provider,
            provider_label: provider.label(),
            id: info.id,
            name: info.name,
            knowledge_count: info.knowledge_count,
            description: info.description,
            capabilities: info.capabilities,
        }
    }
}

/// Shared document summary (provider-tagged), used by reconcile/status code.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInfo {
    pub provider: RagProviderId,
    pub id: String,
    pub title: Option<String>,
    pub status: Option<String>,
}

impl DocInfo {
    pub fn from_weknora(provider: RagProviderId, info: &KnowledgeInfo) -> Self {
        Self {
            provider,
            id: info.id.clone(),
            title: info.title.clone(),
            status: info.parse_status.clone(),
        }
    }
}

/// Provider-dispatching backend. Construct once from settings and reuse.
pub enum RagBackend {
    WeKnora(WeKnoraClient),
    /// Reserved: no adapter yet. Every call returns `not_implemented`.
    RagFlow,
}

impl RagBackend {
    pub fn from_settings(settings: &RagSettings) -> Result<Self, WeKnoraError> {
        match settings.provider {
            RagProviderId::WeKnora => Ok(Self::WeKnora(WeKnoraClient::from_settings(settings)?)),
            RagProviderId::RagFlow => Ok(Self::RagFlow),
        }
    }

    pub fn provider(&self) -> RagProviderId {
        match self {
            Self::WeKnora(_) => RagProviderId::WeKnora,
            Self::RagFlow => RagProviderId::RagFlow,
        }
    }

    pub fn capabilities(&self) -> RagCapabilities {
        self.provider().capabilities()
    }

    /// WeKnora client accessor (only for provider-specific code paths/tests).
    pub fn as_weknora(&self) -> Option<&WeKnoraClient> {
        match self {
            Self::WeKnora(client) => Some(client),
            Self::RagFlow => None,
        }
    }

    /// Error used by every reserved-provider call.
    pub fn not_implemented_error(&self) -> WeKnoraError {
        WeKnoraError::new(
            WeKnoraErrorKind::NotImplemented,
            format!(
                "RAG provider '{}' is not implemented yet; choose WeKnora in Settings → RAG",
                self.provider().label()
            ),
        )
    }

    fn not_implemented<T>(&self) -> Result<T, WeKnoraError> {
        Err(self.not_implemented_error())
    }

    pub fn with_timeout(&self, timeout: std::time::Duration) -> Result<Self, WeKnoraError> {
        match self {
            Self::WeKnora(client) => Ok(Self::WeKnora(client.with_timeout(timeout)?)),
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub fn base_url(&self) -> &str {
        match self {
            Self::WeKnora(client) => client.base_url(),
            Self::RagFlow => "",
        }
    }

    pub async fn list_knowledge_bases(&self) -> Result<Vec<KbSummary>, WeKnoraError> {
        match self {
            Self::WeKnora(client) => Ok(client
                .list_knowledge_bases()
                .await?
                .into_iter()
                .map(|info| KbSummary::from_weknora(RagProviderId::WeKnora, info))
                .collect()),
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn get_knowledge_base(
        &self,
        knowledge_base_id: &str,
    ) -> Result<KnowledgeBaseInfo, WeKnoraError> {
        match self {
            Self::WeKnora(client) => client.get_knowledge_base(knowledge_base_id).await,
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn create_manual(
        &self,
        knowledge_base_id: &str,
        title: &str,
        content: &str,
        tag_ids: &[String],
    ) -> Result<String, WeKnoraError> {
        match self {
            Self::WeKnora(client) => {
                client
                    .create_manual(knowledge_base_id, title, content, tag_ids)
                    .await
            }
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn update_manual(
        &self,
        knowledge_id: &str,
        title: &str,
        content: &str,
        tag_ids: &[String],
    ) -> Result<(), WeKnoraError> {
        match self {
            Self::WeKnora(client) => {
                client
                    .update_manual(knowledge_id, title, content, tag_ids)
                    .await
            }
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn update_metadata(
        &self,
        knowledge_id: &str,
        title: &str,
        custom_metadata: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), WeKnoraError> {
        match self {
            Self::WeKnora(client) => {
                client
                    .update_metadata(knowledge_id, title, custom_metadata)
                    .await
            }
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn get_knowledge(&self, knowledge_id: &str) -> Result<KnowledgeInfo, WeKnoraError> {
        match self {
            Self::WeKnora(client) => client.get_knowledge(knowledge_id).await,
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn list_knowledge(
        &self,
        knowledge_base_id: &str,
        page: u32,
        page_size: u32,
    ) -> Result<Vec<KnowledgeInfo>, WeKnoraError> {
        match self {
            Self::WeKnora(client) => {
                client
                    .list_knowledge(knowledge_base_id, page, page_size)
                    .await
            }
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn delete_knowledge(&self, knowledge_id: &str) -> Result<(), WeKnoraError> {
        match self {
            Self::WeKnora(client) => client.delete_knowledge(knowledge_id).await,
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn batch_delete_knowledge(
        &self,
        knowledge_base_id: &str,
        knowledge_ids: &[String],
    ) -> Result<usize, WeKnoraError> {
        match self {
            Self::WeKnora(client) => {
                client
                    .batch_delete_knowledge(knowledge_base_id, knowledge_ids)
                    .await
            }
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn list_tags(&self, knowledge_base_id: &str) -> Result<Vec<TagInfo>, WeKnoraError> {
        match self {
            Self::WeKnora(client) => client.list_tags(knowledge_base_id).await,
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn create_tag(
        &self,
        knowledge_base_id: &str,
        name: &str,
    ) -> Result<TagInfo, WeKnoraError> {
        match self {
            Self::WeKnora(client) => client.create_tag(knowledge_base_id, name).await,
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn set_knowledge_tags(
        &self,
        knowledge_base_id: &str,
        updates: &std::collections::HashMap<String, Vec<String>>,
    ) -> Result<(), WeKnoraError> {
        match self {
            Self::WeKnora(client) => client.set_knowledge_tags(knowledge_base_id, updates).await,
            Self::RagFlow => self.not_implemented(),
        }
    }

    pub async fn search(&self, request: &SearchRequest) -> Result<Vec<SearchHit>, WeKnoraError> {
        match self {
            Self::WeKnora(client) => client.search(request).await,
            Self::RagFlow => self.not_implemented(),
        }
    }
}
