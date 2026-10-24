//! Read-only WeKnora RAG tools shared by the loopback MCP server
//! (`agentero-web` `/mcp`) and the per-session stdio `agentero-rag` worker.
//!
//! The tools never write to the provider: they resolve the local sync-state mapping
//! (`agentero-core::features::rag::state`) and issue `POST /knowledge-search`.
//! The tested WeKnora API-key scope rejects `knowledge_ids` filters (403 code 1002), so
//! single-paper lookups search KB-wide and filter locally by `knowledge_id`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use agentero_core::error::AppError;
use agentero_core::features::paper::catalog::papers::{self, PaperRecord};
use agentero_core::features::rag::providers::{
    RagBackend, RagProviderId, SearchMode, SearchRequest, WeKnoraErrorKind,
};
use agentero_core::features::rag::render::user_tags;
use agentero_core::features::rag::settings::RagSettings;
use agentero_core::features::rag::state::{self as sync_state, RagSyncState};

/// One search hit content cap returned to agents.
pub const MAX_HIT_CONTENT_CHARS: usize = 4_000;
/// Default `topK` for `rag_search`.
pub const DEFAULT_SEARCH_TOP_K: u32 = 8;
/// Default `topK` for `agentero_rag_paper_context`.
pub const DEFAULT_CONTEXT_TOP_K: u32 = 6;

/// True when the loopback MCP server should advertise the RAG tools.
pub fn mcp_tools_enabled(settings: &RagSettings) -> bool {
    settings.enabled
        && settings.is_configured()
        && settings.provider().is_implemented()
        && settings.tools.expose_to_mcp
}

/// True when the Web host should inject `agentero-rag` into ACP turns.
pub fn agent_tools_enabled(settings: &RagSettings) -> bool {
    settings.enabled
        && settings.is_configured()
        && settings.provider().is_implemented()
        && settings.tools.expose_to_agent
}

// ---------------------------------------------------------------------------
// worker config (written by agentero-host, read by the stdio worker)
// ---------------------------------------------------------------------------

/// Session config for `agentero-web --agentero-rag-mcp <path>`.
///
/// Shape mirrors `agentero_host::context_turn::RagWorkerConfig` (camelCase),
/// written with mode 0600 and removed when the ACP turn ends.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RagWorkerConfig {
    #[serde(default)]
    pub provider: RagProviderId,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub knowledge_base_id: String,
    pub vault_path: String,
    #[serde(default)]
    pub state_path: String,
    #[serde(default)]
    pub rerank: bool,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
}

fn default_timeout_ms() -> u64 {
    15_000
}
fn default_top_k() -> u32 {
    DEFAULT_SEARCH_TOP_K
}

impl RagWorkerConfig {
    pub fn settings(&self) -> RagSettings {
        let mut settings = RagSettings {
            provider: self.provider,
            enabled: true,
            base_url: self.base_url.trim().to_string(),
            api_key: self.api_key.trim().to_string(),
            knowledge_base_id: self.knowledge_base_id.trim().to_string(),
            ..Default::default()
        };
        settings.retrieval.rerank = self.rerank;
        settings.retrieval.timeout_ms = self.timeout_ms.clamp(1_000, 120_000);
        settings.retrieval.top_k = self.top_k.clamp(1, 50);
        settings
    }
}

// ---------------------------------------------------------------------------
// tool arguments / outputs (rmcp JsonSchema DTOs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagSearchArgs {
    /// Natural-language query (semantic + keyword hybrid by default).
    pub query: String,
    /// Hit count, 1-50 (default 8).
    #[serde(default)]
    pub top_k: Option<u32>,
    /// Optional paper id or vault-relative path; restricts hits to that synced paper.
    #[serde(default)]
    pub paper: Option<String>,
    /// Optional catalog tag name (exact match) used as a retrieval filter.
    #[serde(default)]
    pub tag: Option<String>,
    /// `hybrid` (default) | `semantic` | `keyword`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Rerank is off by default (the tested instance can stall 25-40s with it).
    #[serde(default)]
    pub rerank: Option<bool>,
    /// Include provider auto-summary chunks (default false: they repeat content).
    #[serde(default)]
    pub include_summaries: bool,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagPaperContextArgs {
    /// Paper id or vault-relative folder path.
    pub paper: String,
    /// What to look up inside that paper (notes, figure captions, annotations…).
    pub query: String,
    /// Hit count, 1-50 (default 6).
    #[serde(default)]
    pub top_k: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagSyncStatusArgs {
    /// Optional paper id or vault-relative path to report its sync state.
    #[serde(default)]
    pub paper: Option<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagPaperOut {
    /// Paper id usable as `ref` on the other Agentero tools.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Vault-relative paper folder (`papers/...`).
    pub path: String,
    pub title: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bibtex_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagHitOut {
    /// Provider id (`weknora` / `ragflow`).
    pub provider: String,
    pub paper: RagPaperOut,
    pub knowledge_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latex: Option<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagSearchOut {
    pub provider: String,
    pub provider_label: String,
    pub query: String,
    pub hits: Vec<RagHitOut>,
    pub count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded: Option<bool>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagContextHitOut {
    pub knowledge_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latex: Option<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagPaperContextOut {
    pub provider: String,
    pub provider_label: String,
    pub paper: RagPaperOut,
    pub query: String,
    pub hits: Vec<RagContextHitOut>,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagPaperStatusOut {
    #[serde(rename = "ref")]
    pub reference: String,
    pub path: String,
    pub title: String,
    /// `ready` | `indexing` | `failed`; `pending` when not synced yet.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bibtex_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RagSyncStatusOut {
    pub provider: String,
    pub provider_label: String,
    pub enabled: bool,
    pub configured: bool,
    pub total: usize,
    pub synced: usize,
    pub indexing: usize,
    pub failed: usize,
    pub pending: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper: Option<RagPaperStatusOut>,
}

// ---------------------------------------------------------------------------
// runtime
// ---------------------------------------------------------------------------

/// One process/session view of the RAG library: settings + Vault + mapping.
#[derive(Debug, Clone)]
pub struct RagRuntime {
    settings: RagSettings,
    vault: PathBuf,
    state_path: PathBuf,
}

impl RagRuntime {
    pub fn new(settings: RagSettings, vault: &Path) -> Self {
        let state_path = sync_state::state_path(vault, settings.provider());
        Self {
            settings,
            vault: vault.to_path_buf(),
            state_path,
        }
    }

    /// Build a runtime from the stdio-worker session config.
    pub fn from_worker_config(config: &RagWorkerConfig) -> Result<Self, AppError> {
        let base_url = config.base_url.trim();
        let knowledge_base_id = config.knowledge_base_id.trim();
        let vault_path = config.vault_path.trim();
        if base_url.is_empty() || knowledge_base_id.is_empty() || vault_path.is_empty() {
            return Err(AppError::message(
                "agentero-rag: baseUrl, knowledgeBaseId and vaultPath are required",
            ));
        }
        let vault = PathBuf::from(vault_path);
        let state_path = if config.state_path.trim().is_empty() {
            sync_state::state_path(&vault, config.settings().provider())
        } else {
            PathBuf::from(config.state_path.trim())
        };
        Ok(Self {
            settings: config.settings(),
            vault,
            state_path,
        })
    }

    pub fn settings(&self) -> &RagSettings {
        &self.settings
    }

    pub fn vault(&self) -> &Path {
        &self.vault
    }

    /// RAG tools require an explicit opt-in + a configured, implemented provider.
    pub fn enabled(&self) -> bool {
        self.settings.enabled
            && self.settings.is_configured()
            && self.settings.provider().is_implemented()
    }

    fn ensure_enabled(&self) -> Result<(), AppError> {
        if !self.settings.enabled {
            return Err(AppError::domain(
                "rag_disabled",
                "RAG is disabled in Settings → RAG",
            ));
        }
        if !self.settings.provider().is_implemented() {
            return Err(AppError::domain(
                "rag_provider_not_implemented",
                format!(
                    "RAG provider '{}' is not implemented yet; choose WeKnora in Settings → RAG",
                    self.settings.provider().label()
                ),
            ));
        }
        if !self.settings.is_configured() {
            return Err(AppError::domain(
                "rag_not_configured",
                "configure the RAG provider base URL and knowledge base id first (Settings → RAG)",
            ));
        }
        Ok(())
    }

    fn state(&self) -> RagSyncState {
        let provider = self.settings.provider();
        if self.state_path == sync_state::state_path(&self.vault, provider) {
            // Default location: use the loader for v2.0 `weknora/` fallback.
            RagSyncState::load(&self.vault, provider)
        } else {
            match std::fs::read_to_string(&self.state_path) {
                Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
                Err(_) => RagSyncState::default(),
            }
        }
    }

    fn catalog(&self) -> Vec<PaperRecord> {
        papers::list_all(&self.vault).unwrap_or_default()
    }

    fn client(&self) -> Result<RagBackend, AppError> {
        let client = RagBackend::from_settings(&self.settings)?;
        client
            .with_timeout(Duration::from_millis(
                self.settings.retrieval.timeout_ms.max(1_000),
            ))
            .map_err(AppError::from)
    }

    fn resolve_paper(&self, paper: &str) -> Result<PaperRecord, AppError> {
        let lookup = papers::lookup_paper_ref(&self.vault, paper.trim())?;
        match lookup {
            papers::PaperRefLookup::Found(record) => Ok(*record),
            papers::PaperRefLookup::NotFound => {
                Err(AppError::message(format!("paper not found: {paper}")))
            }
            papers::PaperRefLookup::Ambiguous(paths) => Err(AppError::message(format!(
                "paper id '{paper}' is ambiguous ({} matches): {}",
                paths.len(),
                paths.join(", ")
            ))),
        }
    }

    /// `rag_search` implementation.
    pub async fn search(&self, args: RagSearchArgs) -> Result<RagSearchOut, AppError> {
        self.ensure_enabled()?;
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err(AppError::message("query is required"));
        }
        let client = self.client()?;
        let state = self.state();
        let catalog = self.catalog();

        let paper = match args.paper.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(raw) => Some(self.resolve_paper(raw)?),
            None => None,
        };
        let tag_filter_requested = args.tag.as_deref().is_some_and(|tag| !tag.trim().is_empty());
        let query_split = QuerySplit {
            knowledge_base_id: self.settings.knowledge_base_id().to_string(),
            tag_ids: self
                .resolve_tag_ids(&client, args.tag.as_deref())
                .await?,
            tag_filter_requested,
            knowledge_id: paper.as_ref().and_then(|record| {
                state
                    .docs
                    .get(&record.path)
                    .map(|doc| doc.knowledge_id.clone())
                    .filter(|id| !id.is_empty())
            }),
            paper_filter_requested: args.paper.as_deref().is_some_and(|p| !p.trim().is_empty()),
        };
        if query_split.paper_filter_requested && query_split.knowledge_id.is_none() {
            let path = paper.map(|record| record.path).unwrap_or_default();
            return Err(AppError::message(format!(
                "paper is not synced to the RAG library yet: {path}"
            )));
        }
        // A requested-but-unknown tag yields no hits (same as the P1 RPC).
        let provider = self.settings.provider();
        if query_split.tag_filter_requested && query_split.tag_ids.is_empty() {
            return Ok(RagSearchOut {
                provider: provider.id().to_string(),
                provider_label: provider.label().to_string(),
                query,
                hits: Vec::new(),
                count: 0,
                degraded: None,
            });
        }
        let top_k = args
            .top_k
            .unwrap_or(DEFAULT_SEARCH_TOP_K)
            .clamp(1, 50) as usize;
        let rerank = args.rerank.unwrap_or(false);
        let mode = args
            .mode
            .as_deref()
            .map(SearchMode::parse)
            .unwrap_or_default();
        let request = SearchRequest {
            query: query.clone(),
            knowledge_base_ids: vec![query_split.knowledge_base_id],
            knowledge_ids: Vec::new(),
            tag_ids: query_split.tag_ids,
            match_count: top_k.max(1),
            mode,
            rerank,
        };
        let (hits, degraded) = run_search(&client, request, rerank).await?;
        let mapped = map_hits(
            &hits,
            &state,
            &catalog,
            query_split.knowledge_id.as_deref(),
            args.include_summaries,
            provider,
        );
        Ok(RagSearchOut {
            provider: provider.id().to_string(),
            provider_label: provider.label().to_string(),
            query,
            count: mapped.len(),
            hits: mapped,
            degraded,
        })
    }

    /// `agentero_rag_paper_context` implementation: KB-wide search filtered to one
    /// synced paper (API-key scope does not allow `knowledge_ids` filters).
    pub async fn paper_context(
        &self,
        args: RagPaperContextArgs,
    ) -> Result<RagPaperContextOut, AppError> {
        self.ensure_enabled()?;
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err(AppError::message("query is required"));
        }
        let record = self.resolve_paper(&args.paper)?;
        let state = self.state();
        let knowledge_id = state
            .docs
            .get(&record.path)
            .map(|doc| doc.knowledge_id.clone())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                AppError::message(format!(
                    "paper is not synced to the RAG library yet: {}",
                    record.path
                ))
            })?;
        let client = self.client()?;
        let top_k = args
            .top_k
            .unwrap_or(DEFAULT_CONTEXT_TOP_K)
            .clamp(1, 50) as usize;
        // The API-key scope rejects `knowledge_ids`, so fetch a wider KB-wide
        // candidate window before filtering to the requested paper; otherwise
        // a query that matches several papers can crowd it out of the top N.
        let candidates = (top_k.max(1) * 8).clamp(top_k.max(1), 50);
        let request = SearchRequest {
            query: query.clone(),
            knowledge_base_ids: vec![self.settings.knowledge_base_id().to_string()],
            knowledge_ids: Vec::new(),
            tag_ids: Vec::new(),
            match_count: candidates,
            mode: SearchMode::Hybrid,
            rerank: false,
        };
        let (hits, _degraded) = run_search(&client, request, false).await?;
        let paper_out = paper_out(&record, &state);
        let mut context_hits = Vec::new();
        let mut seen = HashSet::new();
        for hit in hits {
            if hit.knowledge_id != knowledge_id {
                continue;
            }
            let dedupe = format!("{}|{}", hit.knowledge_id, hit.content);
            if !seen.insert(dedupe) {
                continue;
            }
            if hit.chunk_type.as_deref() == Some("summary") {
                continue;
            }
            context_hits.push(RagContextHitOut {
                knowledge_id: hit.knowledge_id,
                chunk_type: hit.chunk_type,
                score: hit.score,
                content: truncate_chars(&hit.content, MAX_HIT_CONTENT_CHARS),
                section: detect_section(&hit.content).map(str::to_string),
                latex: paper_out
                    .bibtex_key
                    .as_deref()
                    .map(|key| format!("\\cite{{{key}}}")),
            });
        }
        context_hits.truncate(top_k);
        let provider = self.settings.provider();
        let count = context_hits.len();
        Ok(RagPaperContextOut {
            provider: provider.id().to_string(),
            provider_label: provider.label().to_string(),
            paper: paper_out,
            query,
            hits: context_hits,
            count,
        })
    }

    /// `agentero_rag_sync_status` implementation (local state only, no HTTP).
    pub fn sync_status(&self, args: RagSyncStatusArgs) -> Result<RagSyncStatusOut, AppError> {
        let state = self.state();
        let catalog = self.catalog();
        let counts = state.counts();
        let total = catalog.len();
        let paper = match args.paper.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(raw) => {
                let record = self.resolve_paper(raw)?;
                let doc = state.docs.get(&record.path);
                Some(RagPaperStatusOut {
                    reference: record.id.clone(),
                    path: record.path.clone(),
                    title: record.title.clone(),
                    status: doc
                        .map(|doc| doc.status.clone())
                        .unwrap_or_else(|| "pending".into()),
                    synced_at: doc.and_then(|doc| doc.synced_at.clone()),
                    last_error: doc.and_then(|doc| doc.last_error.clone()),
                    knowledge_id: doc.map(|doc| doc.knowledge_id.clone()),
                    bibtex_key: doc
                        .map(|doc| doc.bibtex_key.clone())
                        .filter(|key| !key.is_empty()),
                })
            }
            None => None,
        };
        let provider = self.settings.provider();
        Ok(RagSyncStatusOut {
            provider: provider.id().to_string(),
            provider_label: provider.label().to_string(),
            enabled: self.settings.enabled,
            configured: self.settings.is_configured(),
            total,
            synced: counts.synced,
            indexing: counts.indexing,
            failed: counts.failed,
            pending: total.saturating_sub(counts.total),
            last_sync_at: state.last_sync_at,
            paper,
        })
    }

    async fn resolve_tag_ids(
        &self,
        client: &RagBackend,
        tag: Option<&str>,
    ) -> Result<Vec<String>, AppError> {
        let Some(tag) = tag.map(str::trim).filter(|tag| !tag.is_empty()) else {
            return Ok(Vec::new());
        };
        let tags = client
            .list_tags(self.settings.knowledge_base_id())
            .await
            .map_err(AppError::from)?;
        Ok(tags
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(tag))
            .map(|candidate| vec![candidate.id.clone()])
            .unwrap_or_default())
    }
}

struct QuerySplit {
    knowledge_base_id: String,
    tag_ids: Vec<String>,
    tag_filter_requested: bool,
    knowledge_id: Option<String>,
    paper_filter_requested: bool,
}

/// Run one search, degrading to un-reranked results on a rerank timeout.
async fn run_search(
    client: &RagBackend,
    request: SearchRequest,
    rerank: bool,
) -> Result<(Vec<agentero_core::features::rag::providers::SearchHit>, Option<bool>), AppError> {
    match client.search(&request).await {
        Ok(hits) => Ok((hits, None)),
        Err(error) if rerank && error.kind == WeKnoraErrorKind::Timeout => {
            let fallback = SearchRequest {
                rerank: false,
                ..request
            };
            let hits = client.search(&fallback).await.map_err(AppError::from)?;
            Ok((hits, Some(true)))
        }
        Err(error) => Err(AppError::from(error)),
    }
}

/// Map WeKnora hits into agent-facing rows (paper mapping + section + key).
fn map_hits(
    hits: &[agentero_core::features::rag::providers::SearchHit],
    state: &RagSyncState,
    catalog: &[PaperRecord],
    knowledge_filter: Option<&str>,
    include_summaries: bool,
    provider: RagProviderId,
) -> Vec<RagHitOut> {
    let by_path: std::collections::HashMap<&str, &PaperRecord> =
        catalog.iter().map(|record| (record.path.as_str(), record)).collect();
    let by_knowledge: std::collections::HashMap<&str, &str> = state
        .docs
        .iter()
        .filter(|(_, doc)| !doc.knowledge_id.is_empty())
        .map(|(path, doc)| (doc.knowledge_id.as_str(), path.as_str()))
        .collect();
    let mut seen = HashSet::new();
    let mut seen_summaries = HashSet::new();
    let mut out = Vec::new();
    for hit in hits {
        if let Some(wanted) = knowledge_filter {
            if hit.knowledge_id != wanted {
                continue;
            }
        }
        let is_summary = hit.chunk_type.as_deref() == Some("summary");
        if is_summary && !include_summaries {
            continue;
        }
        if is_summary && !seen_summaries.insert(hit.knowledge_id.clone()) {
            continue;
        }
        let dedupe = format!("{}|{}", hit.knowledge_id, hit.content);
        if !seen.insert(dedupe) {
            continue;
        }
        let path = by_knowledge
            .get(hit.knowledge_id.as_str())
            .copied()
            .unwrap_or("");
        let record = by_path.get(path).copied();
        let paper = match record {
            Some(record) => paper_out(record, state),
            None => RagPaperOut {
                reference: String::new(),
                path: path.to_string(),
                title: hit.knowledge_title.clone().unwrap_or_default(),
                tags: Vec::new(),
                bibtex_key: None,
            },
        };
        let latex = paper
            .bibtex_key
            .as_deref()
            .map(|key| format!("\\cite{{{key}}}"));
        out.push(RagHitOut {
            provider: provider.id().to_string(),
            paper,
            knowledge_id: hit.knowledge_id.clone(),
            chunk_type: hit.chunk_type.clone(),
            score: hit.score,
            content: truncate_chars(&hit.content, MAX_HIT_CONTENT_CHARS),
            section: detect_section(&hit.content).map(str::to_string),
            latex,
        });
    }
    out
}

fn paper_out(record: &PaperRecord, state: &RagSyncState) -> RagPaperOut {
    let bibtex_key = state
        .docs
        .get(&record.path)
        .map(|doc| doc.bibtex_key.clone())
        .filter(|key| !key.is_empty());
    RagPaperOut {
        reference: record.id.clone(),
        path: record.path.clone(),
        title: record.title.clone(),
        tags: user_tags(record),
        bibtex_key,
    }
}

/// Fixed H2 section detection (WeKnora chunks keep their heading text).
pub fn detect_section(content: &str) -> Option<&'static str> {
    const SECTIONS: &[(&str, &str)] = &[
        ("## Paper body", "paper"),
        ("## User notes", "notes"),
        ("## Annotations and marks", "annotations"),
        ("## Figure and table index", "figure"),
        ("## Abstract", "abstract"),
        ("## Citation (BibTeX / BibLaTeX)", "citation"),
    ];
    SECTIONS
        .iter()
        .find(|(heading, _)| content.contains(heading))
        .map(|(_, section)| *section)
}

/// Trim a hit body to `max_chars` (UTF-8 safe) with an ellipsis marker.
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Parse a 0600 worker config into a runtime.
pub fn load_worker_runtime(path: &Path) -> Result<RagRuntime, AppError> {
    let raw = std::fs::read_to_string(path).map_err(|error| {
        AppError::message(format!(
            "agentero-rag: cannot read config {}: {error}",
            path.display()
        ))
    })?;
    let config: RagWorkerConfig = serde_json::from_str(&raw).map_err(|error| {
        AppError::message(format!("agentero-rag: invalid config json: {error}"))
    })?;
    RagRuntime::from_worker_config(&config)
}

/// Serialize a value for the tool layer (`Json` wrapper); helper for tests.
pub fn to_json_value<T: Serialize>(value: &T) -> Result<Value, AppError> {
    serde_json::to_value(value)
        .map_err(|error| AppError::message(format!("serialize rag tool output: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentero_core::features::paper::catalog::papers::{PaperKind, PaperTag};
    use agentero_core::features::rag::providers::SearchHit;
    use agentero_core::features::rag::state::RagDocState;

    fn record(path: &str) -> PaperRecord {
        let mut record = PaperRecord::local_pdf(
            path.rsplit('/').next().unwrap_or("demo").to_string(),
            "A GaN HEMT paper".into(),
        )
        .at_path(path);
        record.paper_type = PaperKind::Doi;
        record.tags = vec![PaperTag::new("GaN"), PaperTag::new("@zotero:X")];
        record
    }

    fn state_with(path: &str, knowledge_id: &str, key: &str) -> RagSyncState {
        let mut state = RagSyncState::default();
        state.docs.insert(
            path.into(),
            RagDocState {
                knowledge_id: knowledge_id.into(),
                content_hash: "h".into(),
                bibtex_key: key.into(),
                tag_ids: Vec::new(),
                status: "ready".into(),
                synced_at: Some("2026-10-24T00:00:00Z".into()),
                last_error: None,
            },
        );
        state
    }

    fn hit(knowledge_id: &str, chunk: &str, content: &str) -> SearchHit {
        SearchHit {
            id: "chunk".into(),
            content: content.into(),
            knowledge_id: knowledge_id.into(),
            knowledge_title: Some("A GaN HEMT paper".into()),
            chunk_type: Some(chunk.into()),
            score: Some(0.5),
        }
    }

    #[test]
    fn mcp_and_agent_tool_gates_follow_settings() {
        let mut settings = RagSettings {
            provider: RagProviderId::WeKnora,
            enabled: true,
            base_url: "https://kb.example.com".into(),
            knowledge_base_id: "kb".into(),
            ..Default::default()
        };
        assert!(mcp_tools_enabled(&settings));
        assert!(agent_tools_enabled(&settings));
        settings.tools.expose_to_mcp = false;
        assert!(!mcp_tools_enabled(&settings));
        assert!(agent_tools_enabled(&settings));
        settings.enabled = false;
        assert!(!agent_tools_enabled(&settings));
    }

    #[test]
    fn hit_mapping_exposes_paper_tags_key_and_latex() {
        let catalog = vec![record("papers/gan")];
        let state = state_with("papers/gan", "doc-1", "joh");
        let hits = vec![
            hit("doc-1", "text", "## User notes\n用户笔记提到 trapping"),
            hit("doc-1", "summary", "auto summary duplicate"),
            hit("doc-1", "text", "## User notes\n用户笔记提到 trapping"),
        ];
        let mapped = map_hits(&hits, &state, &catalog, Some("doc-1"), false, RagProviderId::WeKnora);
        assert_eq!(mapped.len(), 1, "summary chunks are filtered by default");
        let out = &mapped[0];
        assert_eq!(out.paper.reference, "gan");
        assert_eq!(out.paper.path, "papers/gan");
        assert_eq!(out.paper.tags, vec!["GaN"]);
        assert_eq!(out.paper.bibtex_key.as_deref(), Some("joh"));
        assert_eq!(out.latex.as_deref(), Some("\\cite{joh}"));
        assert_eq!(out.section.as_deref(), Some("notes"));
        assert!(out.content.contains("trapping"));
    }

    #[test]
    fn summaries_are_deduped_per_paper_when_enabled() {
        let catalog = vec![record("papers/gan")];
        let state = state_with("papers/gan", "doc-1", "joh");
        let hits = vec![
            hit("doc-1", "summary", "summary one"),
            hit("doc-1", "summary", "summary two"),
            hit("doc-1", "text", "text chunk"),
        ];
        let mapped = map_hits(&hits, &state, &catalog, None, true, RagProviderId::WeKnora);
        assert_eq!(mapped.len(), 2, "one summary + one text chunk");
        assert!(mapped.iter().any(|hit| hit.chunk_type.as_deref() == Some("summary")));
    }

    #[test]
    fn content_is_truncated_to_the_agent_budget() {
        let long = "x".repeat(MAX_HIT_CONTENT_CHARS + 100);
        let mapped = map_hits(
            &[hit("doc-1", "text", &long)],
            &RagSyncState::default(),
            &[],
            None,
            false,
            RagProviderId::WeKnora,
        );
        assert_eq!(mapped[0].content.chars().count(), MAX_HIT_CONTENT_CHARS + 1);
        assert!(mapped[0].content.ends_with('…'));
    }

    #[test]
    fn sync_status_reports_counts_and_paper_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = RagSettings {
            provider: RagProviderId::WeKnora,
            enabled: true,
            base_url: "https://kb.example.com".into(),
            knowledge_base_id: "kb".into(),
            ..Default::default()
        };
        settings.retrieval.rerank = false;
        let runtime = RagRuntime::new(settings, dir.path());
        // State file is loaded via the configured path; write one mapping row.
        let state_path = agentero_core::features::rag::state::state_path(dir.path(), agentero_core::features::rag::providers::RagProviderId::WeKnora);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        let state = state_with("papers/gan", "doc-1", "joh");
        std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        let status = runtime.sync_status(RagSyncStatusArgs::default()).unwrap();
        assert!(status.enabled && status.configured);
        assert_eq!(status.synced, 1);
        let _ = std::fs::remove_file(state_path);
    }

    #[test]
    fn worker_config_roundtrip_and_validation() {
        let config = RagWorkerConfig {
            provider: RagProviderId::WeKnora,
            base_url: "https://kb.example.com".into(),
            api_key: "secret".into(),
            knowledge_base_id: "kb".into(),
            vault_path: "/tmp/vault".into(),
            state_path: String::new(),
            rerank: false,
            timeout_ms: 15_000,
            top_k: 8,
        };
        let runtime = RagRuntime::from_worker_config(&config).unwrap();
        assert!(runtime.enabled());
        assert_eq!(runtime.settings().api_key(), Some("secret"));
        let mut invalid = config.clone();
        invalid.knowledge_base_id.clear();
        assert!(RagRuntime::from_worker_config(&invalid).is_err());
    }

    #[test]
    fn json_worker_config_parses_host_written_shape() {
        let raw = r#"{"baseUrl":"https://kb.example.com","apiKey":"secret","knowledgeBaseId":"kb","vaultPath":"/tmp/vault","statePath":"/tmp/state.json","rerank":false,"timeoutMs":15000,"topK":8}"#;
        let config: RagWorkerConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(config.base_url, "https://kb.example.com");
        assert_eq!(config.state_path, "/tmp/state.json");
        assert_eq!(config.top_k, 8);
    }
}
