//! WeKnora RAG service for the headless Web host.
//!
//! Owns the sync run (one at a time), the `weknora:sync` / `weknora:changed`
//! events, the optional auto-sync scheduler (60s debounce after Vault writes +
//! 30-min fallback scan) and the read-only RPC helpers. The heavy lifting lives
//! in `agentero_core::features::weknora`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use agentero_core::features::paper::catalog::papers;
use agentero_core::features::system::settings::AppSettingsStore;
use agentero_core::features::weknora::client::{
    KnowledgeBaseInfo, SearchMode, SearchRequest, WeKnoraClient, WeKnoraError,
};
use agentero_core::features::weknora::render::user_tags;
use agentero_core::features::weknora::settings::WeKnoraSettings;
use agentero_core::features::weknora::state::{WeKnoraSyncState, STATUS_INDEXING, STATUS_READY};
use agentero_core::features::weknora::sync::{
    self, SyncOptions, SyncPhase, SyncProgress,
};
use agentero_core::features::weknora::RENDERER_VERSION;

use crate::events::EventBus;

/// Progress event consumed by the settings page (`weknora:sync`).
pub const SYNC_EVENT: &str = "weknora:sync";
/// State change event (`weknora:changed`, payload = `weknora_status` data).
pub const CHANGED_EVENT: &str = "weknora:changed";

/// Auto-sync debounce after Vault writes.
pub const AUTO_SYNC_DEBOUNCE: Duration = Duration::from_secs(60);
/// Hard fallback scan even when auto-sync is on but no writes arrive.
pub const AUTO_SYNC_PERIODIC: Duration = Duration::from_secs(30 * 60);
/// Minimum interval between auto-trigger checks.
const SCHEDULER_TICK: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
struct Inner {
    running: bool,
    cancel: Arc<AtomicBool>,
    progress: SyncProgress,
    last_error: Option<String>,
    last_sync_at: Option<String>,
    /// Last KB name resolved by `weknora_test` / `weknora_status`.
    kb_name: Option<String>,
}

pub struct WeKnoraService {
    settings: Arc<AppSettingsStore>,
    events: EventBus,
    vault: PathBuf,
    inner: Mutex<Inner>,
}

impl WeKnoraService {
    pub fn new(
        settings: Arc<AppSettingsStore>,
        events: EventBus,
        vault: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            events,
            vault,
            inner: Mutex::new(Inner {
                cancel: Arc::new(AtomicBool::new(false)),
                ..Default::default()
            }),
        })
    }

    /// Start the settings listener + auto-sync scheduler (one process-wide task).
    pub fn start(self: &Arc<Self>) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let mut receiver = service.events.subscribe();
            let mut debounce_deadline: Option<Instant> = None;
            let mut last_periodic = Instant::now();
            loop {
                tokio::select! {
                    frame = receiver.recv() => match frame {
                        Ok(frame) => {
                            if frame.event == "settings:changed" {
                                service.emit_changed();
                            } else if frame.event == "vault:file-changed"
                                && service.auto_sync_ready()
                            {
                                debounce_deadline = Some(Instant::now() + AUTO_SYNC_DEBOUNCE);
                            }
                        }
                        // A slow renderer must not kill the scheduler.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => break,
                    },
                    _ = tokio::time::sleep(SCHEDULER_TICK) => {
                        if !service.auto_sync_ready() {
                            last_periodic = Instant::now();
                            debounce_deadline = None;
                            continue;
                        }
                        let settings = service.weknora_settings();
                        let interval = Duration::from_secs(
                            u64::from(settings.sync.auto_sync_minutes.max(1)) * 60,
                        );
                        let debounce_due =
                            debounce_deadline.is_some_and(|deadline| Instant::now() >= deadline);
                        let periodic_due = last_periodic.elapsed() >= interval;
                        if !debounce_due && !periodic_due {
                            continue;
                        }
                        debounce_deadline = None;
                        last_periodic = Instant::now();
                        if service.is_running() {
                            continue;
                        }
                        if let Err(error) = service.start_sync(false, None) {
                            log::warn!("weknora auto-sync not started: {error}");
                        }
                    }
                }
            }
        });
    }

    // ---- settings / state snapshots ---------------------------------------

    fn weknora_settings(&self) -> WeKnoraSettings {
        self.settings.weknora()
    }

    fn mount_settings(&self) -> agentero_core::features::system::settings::BibtexMountSettings {
        self.settings.bibtex_mount()
    }

    fn state(&self) -> WeKnoraSyncState {
        WeKnoraSyncState::load(&self.vault)
    }

    fn catalog(&self) -> Vec<papers::PaperRecord> {
        papers::list_all(&self.vault).unwrap_or_default()
    }

    fn auto_sync_ready(&self) -> bool {
        let settings = self.weknora_settings();
        settings.enabled
            && settings.sync.auto_sync
            && settings.is_configured()
    }

    fn is_running(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.running)
            .unwrap_or(false)
    }

    fn base_client(&self, settings: &WeKnoraSettings) -> Result<WeKnoraClient, WeKnoraError> {
        WeKnoraClient::from_settings(settings)
    }

    // ---- status -----------------------------------------------------------

    /// `weknora_status` payload.
    pub fn status(&self) -> Value {
        let settings = self.weknora_settings();
        let state = self.state();
        let counts = state.counts();
        let catalog = self.catalog();
        let total = catalog.len();
        let pending = total.saturating_sub(counts.total);
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let progress = &inner.progress;
        json!({
            "enabled": settings.enabled,
            "configured": settings.is_configured(),
            "baseUrl": settings.base_url.trim(),
            "knowledgeBaseId": settings.knowledge_base_id.trim(),
            "kbName": inner.kb_name,
            "sync": {
                "total": total,
                "synced": counts.synced,
                "indexing": counts.indexing,
                "failed": counts.failed,
                "pending": pending,
                "lastSyncAt": inner.last_sync_at.clone().or_else(|| state.last_sync_at.clone()),
                "lastError": inner.last_error,
                "running": inner.running,
                "progress": {
                    "phase": progress.phase.as_str(),
                    "done": progress.done,
                    "total": progress.total,
                    "current": progress.current,
                },
            },
            "autoSync": {
                "enabled": settings.sync.auto_sync,
                "intervalMinutes": settings.sync.auto_sync_minutes,
            },
            "tools": {
                "agent": settings.tools.expose_to_agent,
                "mcp": settings.tools.expose_to_mcp,
            },
        })
    }

    fn emit_changed(&self) {
        self.events.emit(CHANGED_EVENT, &self.status());
    }

    fn emit_sync(&self, progress: SyncProgress) {
        self.events.emit(
            SYNC_EVENT,
            &json!({
                "phase": progress.phase.as_str(),
                "done": progress.done,
                "total": progress.total,
                "current": progress.current,
                "created": progress.created,
                "updated": progress.updated,
                "skipped": progress.skipped,
                "deleted": progress.deleted,
                "failed": progress.failed,
                "errors": progress
                    .errors
                    .iter()
                    .map(|error| json!({"paper": error.paper, "error": error.error}))
                    .collect::<Vec<_>>(),
                "running": progress.running,
            }),
        );
    }

    // ---- connection test / KB list ---------------------------------------

    /// Merge RPC overrides (`baseUrl`/`apiKey`/`knowledgeBaseId`) into stored
    /// settings. Masked or missing keys keep the stored secret.
    fn settings_with_overrides(
        &self,
        base_url: Option<&str>,
        api_key: Option<&str>,
        knowledge_base_id: Option<&str>,
    ) -> WeKnoraSettings {
        let mut settings = self.weknora_settings();
        if let Some(base_url) = base_url.map(str::trim).filter(|url| !url.is_empty()) {
            settings.base_url = base_url.to_string();
        }
        if let Some(api_key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
            // A mask means "use the stored key" — leave the stored value.
            if !agentero_core::features::system::settings::is_translate_api_key_mask(api_key) {
                settings.api_key = api_key.to_string();
            }
        }
        if let Some(kb) = knowledge_base_id
            .map(str::trim)
            .filter(|kb| !kb.is_empty())
        {
            settings.knowledge_base_id = kb.to_string();
        }
        settings
    }

    /// `weknora_test` — returns `{ok, kbName?, knowledgeCount?, embeddingModelId?, error?}`.
    pub async fn test_connection(
        self: &Arc<Self>,
        base_url: Option<String>,
        api_key: Option<String>,
        knowledge_base_id: Option<String>,
    ) -> Value {
        let settings = self.settings_with_overrides(
            base_url.as_deref(),
            api_key.as_deref(),
            knowledge_base_id.as_deref(),
        );
        let client = match self.base_client(&settings) {
            Ok(client) => client,
            Err(error) => return json!({ "ok": false, "error": error.to_string() }),
        };
        let kb_id = settings.knowledge_base_id();
        if !kb_id.is_empty() {
            match client.get_knowledge_base(kb_id).await {
                Ok(kb) => {
                    self.cache_kb_name(Some(kb.name.clone()));
                    json!({
                        "ok": true,
                        "kbName": kb.name,
                        "knowledgeCount": kb.knowledge_count,
                        "embeddingModelId": kb.embedding_model_id,
                    })
                }
                Err(error) => json!({ "ok": false, "error": error.to_string() }),
            }
        } else {
            // No KB selected yet: verify credentials by listing knowledge bases.
            match client.list_knowledge_bases().await {
                Ok(kbs) => json!({
                    "ok": true,
                    "kbName": Value::Null,
                    "knowledgeCount": kbs.len(),
                    "embeddingModelId": Value::Null,
                }),
                Err(error) => json!({ "ok": false, "error": error.to_string() }),
            }
        }
    }

    /// `weknora_list_kbs` — `{kbs:[…], error?}`.
    pub async fn list_kbs(
        &self,
        base_url: Option<String>,
        api_key: Option<String>,
    ) -> Value {
        let settings =
            self.settings_with_overrides(base_url.as_deref(), api_key.as_deref(), None);
        let client = match self.base_client(&settings) {
            Ok(client) => client,
            Err(error) => return json!({ "kbs": [], "error": error.to_string() }),
        };
        match client.list_knowledge_bases().await {
            Ok(kbs) => json!({
                "kbs": kbs.iter().map(kb_json).collect::<Vec<_>>(),
                "error": Value::Null,
            }),
            Err(error) => json!({ "kbs": [], "error": error.to_string() }),
        }
    }

    fn cache_kb_name(&self, name: Option<String>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.kb_name = name;
        }
    }

    // ---- sync lifecycle ---------------------------------------------------

    /// Start one sync run. Returns `{started,total,changed,skipped,deleted}`.
    pub fn start_sync(
        self: &Arc<Self>,
        force: bool,
        paper: Option<String>,
    ) -> Result<Value, String> {
        let settings = self.weknora_settings();
        if !settings.enabled {
            return Err("WeKnora RAG is disabled (enable it in Settings → RAG)".into());
        }
        if !settings.is_configured() {
            return Err(
                "configure the WeKnora base URL and knowledge base id first (Settings → RAG)"
                    .into(),
            );
        }
        let catalog = self.catalog();
        let total = match paper
            .as_deref()
            .map(str::trim)
            .filter(|filter| !filter.is_empty())
        {
            Some(filter) => catalog
                .iter()
                .filter(|record| {
                    record.id == filter
                        || record.path == filter
                        || record
                            .path
                            .strip_prefix("papers/")
                            .is_some_and(|suffix| suffix == filter)
                })
                .count(),
            None => catalog.len(),
        };
        {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            if inner.running {
                return Ok(json!({
                    "started": false,
                    "total": total,
                    "changed": 0,
                    "skipped": 0,
                    "deleted": 0,
                }));
            }
            inner.running = true;
            inner.cancel = Arc::new(AtomicBool::new(false));
            inner.last_error = None;
            inner.progress = SyncProgress {
                phase: SyncPhase::Scan,
                total,
                running: true,
                ..Default::default()
            };
        }
        self.emit_sync(self.progress_snapshot());
        self.emit_changed();
        let service = Arc::clone(self);
        tokio::spawn(async move {
            service.run_sync(settings, force, paper).await;
        });
        Ok(json!({
            "started": true,
            "total": total,
            "changed": 0,
            "skipped": 0,
            "deleted": 0,
        }))
    }

    fn progress_snapshot(&self) -> SyncProgress {
        self.inner
            .lock()
            .map(|inner| inner.progress.clone())
            .unwrap_or_default()
    }

    async fn run_sync(
        self: Arc<Self>,
        settings: WeKnoraSettings,
        force: bool,
        paper: Option<String>,
    ) {
        let client = match self.base_client(&settings) {
            Ok(client) => client,
            Err(error) => {
                self.finish_sync(Err(error.to_string()));
                return;
            }
        };
        let mount = self.mount_settings();
        let mut state = self.state();
        let options = SyncOptions::from_settings(&settings, force, paper);
        let cancel = {
            let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            Arc::clone(&inner.cancel)
        };
        let service = Arc::clone(&self);
        let progress = move |progress: SyncProgress| {
            service.record_progress(progress.clone());
            service.emit_sync(progress);
        };
        let result = sync::run(
            &client,
            &self.vault,
            &settings,
            &mount,
            &mut state,
            options,
            &cancel,
            &progress,
        )
        .await;
        match result {
            Ok(report) => {
                self.finish_sync(Ok(report));
            }
            Err(error) => {
                self.finish_sync(Err(error.to_string()));
            }
        }
    }

    fn record_progress(&self, progress: SyncProgress) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.progress = progress;
        }
    }

    fn finish_sync(&self, result: Result<sync::SyncReport, String>) {
        let state = self.state();
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.running = false;
        inner.progress.running = false;
        inner.progress.phase = SyncPhase::Done;
        match result {
            Ok(report) => {
                inner.last_error = report.errors.first().map(|error| {
                    format!("{}: {}", error.paper, error.error)
                });
                inner.last_sync_at = state.last_sync_at.clone();
                inner.progress.done = report.total;
                inner.progress.created = report.created;
                inner.progress.updated = report.updated;
                inner.progress.skipped = report.skipped;
                inner.progress.deleted = report.deleted;
                inner.progress.failed = report.failed;
                inner.progress.errors = report.errors;
                inner.progress.running = false;
            }
            Err(error) => {
                inner.last_error = Some(error.clone());
                inner.progress.running = false;
                inner.progress.errors.push(sync::SyncError {
                    paper: "<sync>".into(),
                    error: error.clone(),
                });
            }
        }
        let final_progress = inner.progress.clone();
        drop(inner);
        log::info!(
            "weknora sync finished: created={} updated={} skipped={} deleted={} failed={}",
            final_progress.created,
            final_progress.updated,
            final_progress.skipped,
            final_progress.deleted,
            final_progress.failed
        );
        self.emit_sync(final_progress);
        self.emit_changed();
    }

    /// Cancel the running sync (the engine checks between documents).
    pub fn cancel(&self) -> Value {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if !inner.running {
            return json!({ "cancelled": false });
        }
        inner.cancel.store(true, Ordering::SeqCst);
        json!({ "cancelled": true })
    }

    // ---- maintenance ------------------------------------------------------

    fn require_client(&self) -> Result<(WeKnoraSettings, WeKnoraClient), String> {
        let settings = self.weknora_settings();
        if !settings.is_configured() {
            return Err(
                "configure the WeKnora base URL and knowledge base id first (Settings → RAG)"
                    .into(),
            );
        }
        let client = WeKnoraClient::from_settings(&settings).map_err(|error| error.to_string())?;
        Ok((settings, client))
    }

    pub async fn reconcile(&self, prune: bool) -> Result<Value, String> {
        let (settings, client) = self.require_client()?;
        let kb = settings.knowledge_base_id().to_string();
        let mut state = self.state();
        let report = sync::reconcile(&client, &self.vault, &kb, &mut state, prune)
            .await
            .map_err(|error| error.to_string())?;
        self.emit_changed();
        Ok(json!({
            "adopted": report.adopted,
            "missing": report.missing,
            "orphans": report.orphans,
            "deleted": report.deleted,
        }))
    }

    pub async fn clear_mapping(&self, delete_docs: bool) -> Result<Value, String> {
        let (settings, client) = self.require_client()?;
        let kb = settings.knowledge_base_id().to_string();
        let mut state = self.state();
        let report = sync::clear_mapping(&client, &self.vault, &kb, &mut state, delete_docs)
            .await
            .map_err(|error| error.to_string())?;
        self.emit_changed();
        Ok(json!({
            "cleared": report.cleared,
            "deleted": report.deleted,
        }))
    }

    // ---- retrieval preview -------------------------------------------------

    /// `weknora_search` — read-only preview used by the settings page / agents.
    pub async fn search(&self, args: SearchArgs) -> Result<Value, String> {
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err("query is required".into());
        }
        let (settings, client) = self.require_client()?;
        let retrieval = settings.retrieval.clone();
        let top_k = args
            .top_k
            .unwrap_or(retrieval.top_k)
            .clamp(1, 50) as usize;
        let rerank = args.rerank.unwrap_or(retrieval.rerank);
        let mode = args
            .mode
            .as_deref()
            .map(SearchMode::parse)
            .unwrap_or_default();
        let state = self.state();
        let catalog = self.catalog();
        let by_path: HashMap<&str, &papers::PaperRecord> =
            catalog.iter().map(|record| (record.path.as_str(), record)).collect();
        let by_knowledge: HashMap<&str, (&str, &str)> = state
            .docs
            .iter()
            .map(|(path, doc)| {
                (
                    doc.knowledge_id.as_str(),
                    (path.as_str(), doc.bibtex_key.as_str()),
                )
            })
            .collect();

        // Optional single-paper restriction. The tested API-key scope rejects
        // `knowledge_ids` filters ("not allowed without a verified knowledge
        // base"), so search KB-wide and filter by the mapped knowledge id.
        let mut paper_knowledge_id: Option<String> = None;
        if let Some(paper) = args.paper.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            let resolved = papers::lookup_paper_ref(&self.vault, paper)
                .ok()
                .and_then(|lookup| match lookup {
                    papers::PaperRefLookup::Found(record) => Some(record.path),
                    _ => None,
                })
                .unwrap_or_else(|| paper.to_string());
            paper_knowledge_id = Some(
                state
                    .docs
                    .get(&resolved)
                    .map(|doc| doc.knowledge_id.clone())
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| format!("paper is not synced to WeKnora yet: {resolved}"))?,
            );
        }

        // Optional tag-name filter (resolved through the KB tag list).
        let mut tag_ids = Vec::new();
        if let Some(tag) = args.tag.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            let tags = client.list_tags(settings.knowledge_base_id()).await.map_err(|error| {
                format!("resolve WeKnora tag {tag}: {error}")
            })?;
            if let Some(found) = tags
                .iter()
                .find(|candidate| candidate.name.eq_ignore_ascii_case(tag))
            {
                tag_ids.push(found.id.clone());
            } else {
                return Ok(json!({ "hits": [], "count": 0 }));
            }
        }

        let request = SearchRequest {
            query,
            knowledge_base_ids: vec![settings.knowledge_base_id().to_string()],
            knowledge_ids: Vec::new(),
            tag_ids,
            match_count: top_k,
            mode,
            rerank,
        };
        let client = client
            .with_timeout(Duration::from_millis(retrieval.timeout_ms.max(1_000)))
            .map_err(|error| error.to_string())?;
        let mut degraded = false;
        let hits = match client.search(&request).await {
            Ok(hits) => hits,
            Err(error) if error.kind == agentero_core::features::weknora::client::WeKnoraErrorKind::Timeout && rerank => {
                degraded = true;
                let fallback = SearchRequest {
                    rerank: false,
                    ..request
                };
                client.search(&fallback).await.map_err(|error| error.to_string())?
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut seen = std::collections::HashSet::new();
        let mut mapped = Vec::new();
        for hit in hits {
            if let Some(wanted) = paper_knowledge_id.as_deref() {
                if hit.knowledge_id != wanted {
                    continue;
                }
            }
            let dedupe_key = format!("{}|{}", hit.knowledge_id, hit.content);
            if !seen.insert(dedupe_key) {
                continue;
            }
            let (path, bibtex_key) = by_knowledge
                .get(hit.knowledge_id.as_str())
                .copied()
                .unwrap_or(("", ""));
            let record = by_path.get(path).copied();
            let title = record
                .map(|record| record.title.clone())
                .or(hit.knowledge_title.clone())
                .unwrap_or_default();
            let tags = record.map(user_tags).unwrap_or_default();
            let latex = if bibtex_key.is_empty() {
                Value::Null
            } else {
                Value::String(format!("\\cite{{{bibtex_key}}}"))
            };
            mapped.push(json!({
                "paper": {
                    "ref": record.map(|record| record.id.clone()).unwrap_or_default(),
                    "path": path,
                    "title": title,
                    "tags": tags,
                    "bibtexKey": if bibtex_key.is_empty() { Value::Null } else { Value::String(bibtex_key.to_string()) },
                },
                "knowledgeId": hit.knowledge_id,
                "chunkType": hit.chunk_type,
                "score": hit.score,
                "content": truncate_chars(&hit.content, 4_000),
                "section": detect_section(&hit.content),
                "latex": latex,
            }));
        }
        let count = mapped.len();
        let mut payload = json!({ "hits": mapped, "count": count });
        if degraded {
            payload["degraded"] = json!(true);
        }
        Ok(payload)
    }
}

/// `weknora_search` parameters.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchArgs {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub top_k: Option<u32>,
    #[serde(default)]
    pub paper: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub rerank: Option<bool>,
}

fn kb_json(kb: &KnowledgeBaseInfo) -> Value {
    json!({
        "id": kb.id,
        "name": kb.name,
        "knowledgeCount": kb.knowledge_count,
        "description": kb.description,
        "capabilities": kb.capabilities,
    })
}

/// Fixed H2 section detection (chunks keep their heading text upstream).
fn detect_section(content: &str) -> Option<&'static str> {
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

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Status helpers shared by tests: derive synced/pending counters.
pub fn state_counters(vault: &Path) -> (usize, usize, usize, usize) {
    let state = WeKnoraSyncState::load(vault);
    let counts = state.counts();
    let catalog = papers::list_all(vault).map(|rows| rows.len()).unwrap_or(0);
    (
        catalog,
        counts.synced,
        counts.indexing,
        counts.failed,
    )
}

/// Renderer version reported by status diagnostics (kept in one place).
pub const WEKNORA_RENDERER_VERSION: u32 = RENDERER_VERSION;

/// Ensure the status payload exposes the same renderer version as the engine.
pub fn renderer_version() -> u32 {
    WEKNORA_RENDERER_VERSION
}

/// Convenience for the MCP/agent tool layer later: synced ready paper paths.
pub fn synced_paths(vault: &Path) -> Vec<String> {
    let state = WeKnoraSyncState::load(vault);
    state
        .docs
        .iter()
        .filter(|(_, doc)| doc.status == STATUS_READY || doc.status == STATUS_INDEXING)
        .map(|(path, _)| path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentero_core::features::system::settings::AppSettings;

    fn service_for_tests() -> (
        tempfile::TempDir,
        Arc<WeKnoraService>,
        EventBus,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(AppSettingsStore::for_tests(AppSettings::default()));
        let events = EventBus::new();
        let service = WeKnoraService::new(
            store,
            events.clone(),
            dir.path().to_path_buf(),
        );
        (dir, service, events)
    }

    #[test]
    fn status_payload_matches_the_frontend_contract() {
        let (_dir, service, _events) = service_for_tests();
        let status = service.status();
        assert_eq!(status["enabled"], false);
        assert_eq!(status["configured"], false);
        assert_eq!(status["kbName"], Value::Null);
        for key in ["baseUrl", "knowledgeBaseId", "sync", "autoSync", "tools"] {
            assert!(status.get(key).is_some(), "missing {key}: {status}");
        }
        assert_eq!(status["sync"]["total"], 0);
        assert_eq!(status["sync"]["synced"], 0);
        assert_eq!(status["sync"]["running"], false);
        assert_eq!(status["sync"]["progress"]["phase"], "scan");
        assert_eq!(status["autoSync"]["enabled"], false);
        assert_eq!(status["autoSync"]["intervalMinutes"], 30);
        assert_eq!(status["tools"]["agent"], true);
        assert_eq!(status["tools"]["mcp"], true);
    }

    #[tokio::test]
    async fn sync_and_changed_events_carry_the_documented_payloads() {
        let (_dir, service, events) = service_for_tests();
        let mut frames = events.subscribe();
        service.emit_changed();
        let changed = frames.recv().await.unwrap();
        assert_eq!(changed.event, CHANGED_EVENT);
        assert_eq!(changed.payload["enabled"], false);
        assert!(changed.payload["sync"].is_object());

        service.emit_sync(SyncProgress {
            phase: SyncPhase::Sync,
            done: 2,
            total: 6,
            current: Some("papers/alpha".into()),
            created: 1,
            updated: 1,
            skipped: 0,
            deleted: 0,
            failed: 0,
            errors: vec![sync::SyncError {
                paper: "papers/beta".into(),
                error: "boom".into(),
            }],
            running: true,
        });
        let sync_frame = frames.recv().await.unwrap();
        assert_eq!(sync_frame.event, SYNC_EVENT);
        assert_eq!(sync_frame.payload["phase"], "sync");
        assert_eq!(sync_frame.payload["done"], 2);
        assert_eq!(sync_frame.payload["total"], 6);
        assert_eq!(sync_frame.payload["current"], "papers/alpha");
        assert_eq!(sync_frame.payload["running"], true);
        assert_eq!(sync_frame.payload["errors"][0]["paper"], "papers/beta");
    }

    #[test]
    fn settings_overrides_keep_the_stored_secret_for_masks() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = AppSettings::default();
        settings.weknora.base_url = "https://stored.example.com".into();
        settings.weknora.knowledge_base_id = "kb-stored".into();
        settings.weknora.api_key = "stored-secret".into();
        let store = Arc::new(AppSettingsStore::for_tests(settings));
        let service = WeKnoraService::new(
            store,
            EventBus::new(),
            dir.path().to_path_buf(),
        );
        let masked = service.settings_with_overrides(
            Some("https://override.example.com"),
            Some("*********"),
            Some("kb-override"),
        );
        assert_eq!(masked.base_url, "https://override.example.com");
        assert_eq!(masked.knowledge_base_id, "kb-override");
        assert_eq!(masked.api_key(), Some("stored-secret"));

        let replaced = service.settings_with_overrides(None, Some("new-secret"), None);
        assert_eq!(replaced.api_key(), Some("new-secret"));

        let untouched = service.settings_with_overrides(Some("  "), Some(""), None);
        assert_eq!(untouched.base_url, "https://stored.example.com");
        assert_eq!(untouched.api_key(), Some("stored-secret"));
    }

    #[test]
    fn disabled_or_unconfigured_sync_reports_a_readable_error() {
        let (_dir, service, _events) = service_for_tests();
        let error = service.start_sync(false, None).unwrap_err();
        assert!(error.contains("disabled"), "{error}");
    }

    #[test]
    fn section_detection_matches_renderer_headings() {
        assert_eq!(
            detect_section("intro\n## User notes\nbody"),
            Some("notes")
        );
        assert_eq!(
            detect_section("## Annotations and marks\n- page 3"),
            Some("annotations")
        );
        assert_eq!(detect_section("just text"), None);
    }

    #[test]
    fn truncation_keeps_utf8_and_marks_overflow() {
        let text = "汉".repeat(10);
        let out = truncate_chars(&text, 4);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
        assert_eq!(truncate_chars("short", 100), "short");
    }

    #[test]
    fn renderer_version_is_pinned() {
        assert_eq!(renderer_version(), 1);
    }
}
