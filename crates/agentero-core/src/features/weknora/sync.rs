//! WeKnora sync engine: scan → render → diff → execute → tags → index wait →
//! delete, plus reconcile / clear-mapping maintenance.
//!
//! The engine is host-agnostic: it talks to a [`KnowledgeApi`] (implemented by
//! [`WeKnoraClient`], mocked in tests) and persists [`WeKnoraSyncState`] after
//! every document, so a crash leaves a resumable mapping.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::error::AppError;
use crate::features::paper::catalog::bibtex_export::{self, BibEntry};
use crate::features::paper::catalog::papers::{self, PaperRecord};
use crate::features::system::settings::BibtexMountSettings;
use crate::features::weknora::client::{
    KnowledgeInfo, SearchHit, TagInfo, WeKnoraClient, WeKnoraError,
};
use crate::features::weknora::render::{
    content_hash, render_paper, user_tags, RenderOptions, RenderOutput,
};
use crate::features::weknora::settings::WeKnoraSettings;
use crate::features::weknora::state::{
    self, vault_key, WeKnoraDocState, WeKnoraSyncState, STATUS_FAILED, STATUS_INDEXING,
    STATUS_READY,
};
use crate::features::weknora::RENDERER_VERSION;
use crate::time::now_rfc3339_millis;

/// WeKnora REST operations the sync engine needs. Implemented by
/// [`WeKnoraClient`]; tests use an in-memory mock.
#[async_trait]
pub trait KnowledgeApi: Send + Sync {
    async fn create_manual(
        &self,
        knowledge_base_id: &str,
        title: &str,
        content: &str,
        tag_ids: &[String],
    ) -> Result<String, WeKnoraError>;
    async fn update_manual(
        &self,
        knowledge_id: &str,
        title: &str,
        content: &str,
        tag_ids: &[String],
    ) -> Result<(), WeKnoraError>;
    async fn update_metadata(
        &self,
        knowledge_id: &str,
        title: &str,
        custom_metadata: &Map<String, Value>,
    ) -> Result<(), WeKnoraError>;
    async fn get_knowledge(&self, knowledge_id: &str) -> Result<KnowledgeInfo, WeKnoraError>;
    async fn delete_knowledge(&self, knowledge_id: &str) -> Result<(), WeKnoraError>;
    async fn batch_delete_knowledge(
        &self,
        knowledge_base_id: &str,
        ids: &[String],
    ) -> Result<usize, WeKnoraError>;
    async fn list_tags(&self, knowledge_base_id: &str) -> Result<Vec<TagInfo>, WeKnoraError>;
    async fn create_tag(
        &self,
        knowledge_base_id: &str,
        name: &str,
    ) -> Result<TagInfo, WeKnoraError>;
    async fn set_knowledge_tags(
        &self,
        knowledge_base_id: &str,
        updates: &HashMap<String, Vec<String>>,
    ) -> Result<(), WeKnoraError>;
    async fn list_knowledge(
        &self,
        knowledge_base_id: &str,
        page: u32,
        page_size: u32,
    ) -> Result<Vec<KnowledgeInfo>, WeKnoraError>;
    async fn search(
        &self,
        request: &crate::features::weknora::client::SearchRequest,
    ) -> Result<Vec<SearchHit>, WeKnoraError>;
}

#[async_trait]
impl KnowledgeApi for WeKnoraClient {
    async fn create_manual(
        &self,
        knowledge_base_id: &str,
        title: &str,
        content: &str,
        tag_ids: &[String],
    ) -> Result<String, WeKnoraError> {
        WeKnoraClient::create_manual(self, knowledge_base_id, title, content, tag_ids).await
    }
    async fn update_manual(
        &self,
        knowledge_id: &str,
        title: &str,
        content: &str,
        tag_ids: &[String],
    ) -> Result<(), WeKnoraError> {
        WeKnoraClient::update_manual(self, knowledge_id, title, content, tag_ids).await
    }
    async fn update_metadata(
        &self,
        knowledge_id: &str,
        title: &str,
        custom_metadata: &Map<String, Value>,
    ) -> Result<(), WeKnoraError> {
        WeKnoraClient::update_metadata(self, knowledge_id, title, custom_metadata).await
    }
    async fn get_knowledge(&self, knowledge_id: &str) -> Result<KnowledgeInfo, WeKnoraError> {
        WeKnoraClient::get_knowledge(self, knowledge_id).await
    }
    async fn delete_knowledge(&self, knowledge_id: &str) -> Result<(), WeKnoraError> {
        WeKnoraClient::delete_knowledge(self, knowledge_id).await
    }
    async fn batch_delete_knowledge(
        &self,
        knowledge_base_id: &str,
        ids: &[String],
    ) -> Result<usize, WeKnoraError> {
        WeKnoraClient::batch_delete_knowledge(self, knowledge_base_id, ids).await
    }
    async fn list_tags(&self, knowledge_base_id: &str) -> Result<Vec<TagInfo>, WeKnoraError> {
        WeKnoraClient::list_tags(self, knowledge_base_id).await
    }
    async fn create_tag(
        &self,
        knowledge_base_id: &str,
        name: &str,
    ) -> Result<TagInfo, WeKnoraError> {
        WeKnoraClient::create_tag(self, knowledge_base_id, name).await
    }
    async fn set_knowledge_tags(
        &self,
        knowledge_base_id: &str,
        updates: &HashMap<String, Vec<String>>,
    ) -> Result<(), WeKnoraError> {
        WeKnoraClient::set_knowledge_tags(self, knowledge_base_id, updates).await
    }
    async fn list_knowledge(
        &self,
        knowledge_base_id: &str,
        page: u32,
        page_size: u32,
    ) -> Result<Vec<KnowledgeInfo>, WeKnoraError> {
        WeKnoraClient::list_knowledge(self, knowledge_base_id, page, page_size).await
    }
    async fn search(
        &self,
        request: &crate::features::weknora::client::SearchRequest,
    ) -> Result<Vec<SearchHit>, WeKnoraError> {
        WeKnoraClient::search(self, request).await
    }
}

/// One sync trigger's options.
#[derive(Debug, Clone)]
pub struct SyncOptions {
    /// Ignore stored hashes and update every rendered document.
    pub force: bool,
    /// Limit the run to one paper (id or vault-relative path). Deletions are
    /// not performed for single-paper runs.
    pub paper: Option<String>,
    pub max_bytes: usize,
    pub index_timeout: Duration,
    pub index_poll_interval: Duration,
    pub concurrency: usize,
}

impl SyncOptions {
    pub fn from_settings(settings: &WeKnoraSettings, force: bool, paper: Option<String>) -> Self {
        Self {
            force,
            paper,
            max_bytes: crate::features::weknora::MAX_DOCUMENT_BYTES,
            index_timeout: Duration::from_secs(settings.advanced.index_timeout_secs.max(10)),
            index_poll_interval: Duration::from_secs(1),
            concurrency: settings.advanced.concurrency.clamp(1, 4) as usize,
        }
    }
}

/// Sync phases reported through `weknora:sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    Scan,
    Render,
    Sync,
    Index,
    Done,
}

impl SyncPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Render => "render",
            Self::Sync => "sync",
            Self::Index => "index",
            Self::Done => "done",
        }
    }
}

/// One per-document failure (paper path + readable error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncError {
    pub paper: String,
    pub error: String,
}

/// Progress payload emitted as `weknora:sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncProgress {
    pub phase: SyncPhase,
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub deleted: usize,
    pub failed: usize,
    pub errors: Vec<SyncError>,
    pub running: bool,
}

impl Default for SyncProgress {
    fn default() -> Self {
        Self {
            phase: SyncPhase::Scan,
            done: 0,
            total: 0,
            current: None,
            created: 0,
            updated: 0,
            skipped: 0,
            deleted: 0,
            failed: 0,
            errors: Vec::new(),
            running: false,
        }
    }
}

pub type ProgressFn<'a> = &'a (dyn Fn(SyncProgress) + Send + Sync);

/// Final result of one sync run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub total: usize,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub deleted: usize,
    pub failed: usize,
    pub errors: Vec<SyncError>,
    /// True when the run stopped early because cancellation was requested.
    pub cancelled: bool,
}

/// Diff between the rendered catalog and the stored mapping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncDiff {
    pub create: Vec<String>,
    pub update: Vec<String>,
    pub skip: Vec<String>,
    pub delete: Vec<String>,
}

/// A rendered document ready to be written.
#[derive(Debug, Clone)]
pub struct RenderedDoc {
    pub record: PaperRecord,
    pub rendered: RenderOutput,
    pub hash: String,
}

/// Compute create/update/skip sets from rendered docs; `delete` is derived from
/// stored docs that no longer exist in `rendered` (only when `allow_delete`).
pub fn diff(
    rendered: &BTreeMap<String, RenderedDoc>,
    state: &WeKnoraSyncState,
    force: bool,
    allow_delete: bool,
) -> SyncDiff {
    let mut plan = SyncDiff::default();
    for (path, doc) in rendered {
        match state.docs.get(path) {
            Some(existing) if !force && existing.content_hash == doc.hash => {
                plan.skip.push(path.clone())
            }
            Some(_) => plan.update.push(path.clone()),
            None => plan.create.push(path.clone()),
        }
    }
    if allow_delete {
        for path in state.docs.keys() {
            if !rendered.contains_key(path) {
                plan.delete.push(path.clone());
            }
        }
    }
    plan
}

/// `custom_metadata` written to every managed knowledge document.
pub fn custom_metadata(vault: &Path, doc: &RenderedDoc) -> Map<String, Value> {
    let record = &doc.record;
    let mut metadata = Map::new();
    metadata.insert("agentero_managed".into(), Value::String("true".into()));
    metadata.insert(
        "agentero_vault".into(),
        Value::String(vault_key(vault)),
    );
    metadata.insert(
        "agentero_paper_id".into(),
        Value::String(record.id.clone()),
    );
    metadata.insert("agentero_path".into(), Value::String(record.path.clone()));
    metadata.insert(
        "agentero_title".into(),
        Value::String(record.title.clone()),
    );
    if let Some(doi) = record
        .doi
        .as_deref()
        .map(str::trim)
        .filter(|doi| !doi.is_empty())
    {
        metadata.insert("agentero_doi".into(), Value::String(doi.to_string()));
    }
    if let Some(year) = record.year {
        metadata.insert("agentero_year".into(), Value::String(year.to_string()));
    }
    let tags = user_tags(record);
    if !tags.is_empty() {
        metadata.insert("agentero_tags".into(), Value::String(tags.join(",")));
    }
    metadata.insert("agentero_kind".into(), Value::String("paper".into()));
    metadata.insert(
        "agentero_renderer".into(),
        Value::String(RENDERER_VERSION.to_string()),
    );
    metadata.insert(
        "agentero_hash".into(),
        Value::String(doc.hash.clone()),
    );
    metadata.insert(
        "agentero_source_updated_at".into(),
        Value::String(record.updated_at.clone()),
    );
    for (key, present) in [
        ("agentero_has_body", doc.rendered.includes.paper_body),
        ("agentero_notes", doc.rendered.includes.notes),
        ("agentero_marks", doc.rendered.includes.marks),
        ("agentero_layout", doc.rendered.includes.figure_index),
    ] {
        metadata.insert(
            key.into(),
            Value::String(if present { "true" } else { "false" }.into()),
        );
    }
    metadata.insert(
        "agentero_pdf_excluded".into(),
        Value::String("true".into()),
    );
    metadata
}

fn document_title(doc: &RenderedDoc) -> String {
    let title = doc.record.title.trim();
    if title.is_empty() {
        doc.record.id.clone()
    } else {
        title.to_string()
    }
}

/// Scan the catalog and render every selected paper.
///
/// Runs on the blocking pool from `run()`: file IO + BibTeX serialization are
/// synchronous and should not stall the async runtime.
fn scan_and_render(
    vault: &Path,
    settings: &WeKnoraSettings,
    mount: &BibtexMountSettings,
    options: &SyncOptions,
    paper_filter: Option<&str>,
) -> Result<BTreeMap<String, RenderedDoc>, AppError> {
    let records = papers::list_all(vault)?;
    let bib_entries = bibtex_export::entries(vault, mount).unwrap_or_else(|error| {
        log::warn!("weknora: cannot render mount BibTeX index: {error}");
        Vec::new()
    });
    let bib_by_path: HashMap<String, BibEntry> = bib_entries
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();
    let render_options = RenderOptions::from_settings(&settings.sync);
    let mut rendered = BTreeMap::new();
    for record in records {
        if let Some(filter) = paper_filter {
            let matches = record.id == filter
                || record.path == filter
                || record
                    .path
                    .strip_prefix("papers/")
                    .is_some_and(|suffix| suffix == filter);
            if !matches {
                continue;
            }
        }
        let bib = bib_by_path.get(&record.path);
        let output = render_paper(vault, &record, bib, mount, render_options)?;
        let hash = content_hash(&render_options, &output.markdown);
        rendered.insert(
            record.path.clone(),
            RenderedDoc {
                record,
                rendered: output,
                hash,
            },
        );
    }
    let _ = options;
    Ok(rendered)
}

/// Resolve every catalog tag to a WeKnora tag id, creating missing tags.
async fn prepare_tags(
    api: &dyn KnowledgeApi,
    knowledge_base_id: &str,
    docs: &[&RenderedDoc],
) -> Result<HashMap<String, TagInfo>, WeKnoraError> {
    let mut by_name: HashMap<String, TagInfo> = HashMap::new();
    for tag in api.list_tags(knowledge_base_id).await? {
        if !tag.name.is_empty() {
            by_name.insert(tag.name.to_ascii_lowercase(), tag);
        }
    }
    for doc in docs {
        for name in user_tags(&doc.record) {
            let key = name.to_ascii_lowercase();
            if by_name.contains_key(&key) {
                continue;
            }
            let created = api.create_tag(knowledge_base_id, &name).await?;
            by_name.insert(key, created);
        }
    }
    Ok(by_name)
}

fn tag_ids_for(doc: &RenderedDoc, tags: &HashMap<String, TagInfo>) -> Vec<String> {
    let mut ids = Vec::new();
    for name in user_tags(&doc.record) {
        if let Some(tag) = tags.get(&name.to_ascii_lowercase()) {
            if !tag.id.is_empty() && !ids.contains(&tag.id) {
                ids.push(tag.id.clone());
            }
        }
    }
    ids
}

/// Wait for one document to become searchable; never blocks the run forever.
async fn wait_for_index(
    api: &dyn KnowledgeApi,
    knowledge_id: &str,
    timeout: Duration,
    poll_interval: Duration,
    cancel: &AtomicBool,
) -> (String, Option<String>) {
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            return (STATUS_INDEXING.into(), Some("sync cancelled".into()));
        }
        match api.get_knowledge(knowledge_id).await {
            Ok(info) => {
                if info.is_failed() {
                    let status = info
                        .parse_status
                        .clone()
                        .unwrap_or_else(|| "failed".into());
                    return (
                        STATUS_FAILED.into(),
                        Some(format!("WeKnora indexing failed ({status})")),
                    );
                }
                if info.is_indexed() {
                    return (STATUS_READY.into(), None);
                }
            }
            Err(error) => {
                if error.is_auth() {
                    return (STATUS_FAILED.into(), Some(error.to_string()));
                }
            }
        }
        if started.elapsed() >= timeout {
            return (
                STATUS_INDEXING.into(),
                Some(format!(
                    "indexing not finished after {}s; left for reconcile",
                    timeout.as_secs()
                )),
            );
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Execute one create/update job; returns the persisted mapping.
async fn execute_doc(
    api: &dyn KnowledgeApi,
    vault: &Path,
    knowledge_base_id: &str,
    doc: &RenderedDoc,
    existing: Option<&WeKnoraDocState>,
    tag_ids: &[String],
    options: &SyncOptions,
    cancel: &AtomicBool,
) -> WeKnoraDocState {
    let metadata = custom_metadata(vault, doc);
    let title = document_title(doc);
    let created = existing.is_none();
    let outcome: Result<String, WeKnoraError> = if created {
        api.create_manual(
            knowledge_base_id,
            &title,
            &doc.rendered.markdown,
            tag_ids,
        )
        .await
    } else {
        let knowledge_id = existing.map(|doc| doc.knowledge_id.clone()).unwrap_or_default();
        match api
            .update_manual(
                &knowledge_id,
                &title,
                &doc.rendered.markdown,
                tag_ids,
            )
            .await
        {
            Ok(()) => Ok(knowledge_id),
            Err(error) => Err(error),
        }
    };
    let knowledge_id = match outcome {
        Ok(id) => id,
        Err(error) => {
            return WeKnoraDocState {
                knowledge_id: existing
                    .map(|doc| doc.knowledge_id.clone())
                    .unwrap_or_default(),
                content_hash: existing
                    .map(|doc| doc.content_hash.clone())
                    .unwrap_or_default(),
                bibtex_key: doc.rendered.bibtex_key.clone(),
                tag_ids: existing
                    .map(|doc| doc.tag_ids.clone())
                    .unwrap_or_default(),
                status: STATUS_FAILED.into(),
                synced_at: existing.and_then(|doc| doc.synced_at.clone()),
                last_error: Some(error.to_string()),
            };
        }
    };
    if let Err(error) = api
        .update_metadata(&knowledge_id, &title, &metadata)
        .await
    {
        log::warn!(
            "weknora: {} metadata update failed: {error}",
            doc.record.path
        );
    }
    let (status, index_error) = wait_for_index(
        api,
        &knowledge_id,
        options.index_timeout,
        options.index_poll_interval,
        cancel,
    )
    .await;
    WeKnoraDocState {
        knowledge_id,
        content_hash: doc.hash.clone(),
        bibtex_key: doc.rendered.bibtex_key.clone(),
        tag_ids: tag_ids.to_vec(),
        status,
        synced_at: Some(now_rfc3339_millis()),
        last_error: index_error,
    }
}

/// Run one full sync.
pub async fn run(
    api: &dyn KnowledgeApi,
    vault: &Path,
    settings: &WeKnoraSettings,
    mount: &BibtexMountSettings,
    state: &mut WeKnoraSyncState,
    options: SyncOptions,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<SyncReport, AppError> {
    if !settings.is_configured() {
        return Err(AppError::domain(
            "weknora_not_configured",
            "configure the WeKnora base URL and knowledge base id first",
        ));
    }
    let knowledge_base_id = settings.knowledge_base_id().to_string();
    state.prepare_for_kb(&knowledge_base_id);
    state.renderer_version = RENDERER_VERSION;
    let mut report = SyncReport::default();
    let mut current = SyncProgress {
        phase: SyncPhase::Scan,
        running: true,
        ..Default::default()
    };
    progress(current.clone());

    current.phase = SyncPhase::Render;
    progress(current.clone());
    let vault_owned = vault.to_path_buf();
    let settings_owned = settings.clone();
    let mount_owned = mount.clone();
    let options_owned = options.clone();
    let filter = options.paper.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        scan_and_render(
            &vault_owned,
            &settings_owned,
            &mount_owned,
            &options_owned,
            filter.as_deref(),
        )
    })
    .await
    .map_err(|error| AppError::message(format!("weknora render task failed: {error}")))??;
    let total = rendered.len();
    report.total = total;
    current.total = total;
    current.phase = SyncPhase::Sync;

    let plan = diff(&rendered, state, options.force, options.paper.is_none());
    report.skipped = plan.skip.len();
    current.skipped = plan.skip.len();
    progress(current.clone());

    // Resolve tag ids up front (creates missing WeKnora tags exactly once).
    let job_paths: Vec<String> = plan
        .create
        .iter()
        .chain(plan.update.iter())
        .cloned()
        .collect();
    let job_docs: Vec<&RenderedDoc> = job_paths
        .iter()
        .filter_map(|path| rendered.get(path))
        .collect();
    let tag_map = if job_docs.is_empty() {
        HashMap::new()
    } else {
        prepare_tags(api, &knowledge_base_id, &job_docs)
            .await
            .map_err(|error| {
                AppError::from(WeKnoraError {
                    kind: error.kind,
                    status: error.status,
                    code: error.code,
                    message: format!("prepare WeKnora tags: {}", error.message),
                })
            })?
    };

    let mut tag_updates: HashMap<String, Vec<String>> = HashMap::new();
    let mut done = 0usize;
    let mut errors: Vec<SyncError> = Vec::new();
    for batch in job_paths.chunks(options.concurrency.max(1)) {
        if cancel.load(Ordering::SeqCst) {
            report.cancelled = true;
            break;
        }
        let mut futures = Vec::new();
        for path in batch {
            let Some(doc) = rendered.get(path) else {
                continue;
            };
            let tag_ids = tag_ids_for(doc, &tag_map);
            let existing = state.docs.get(path).cloned();
            let api = api;
            let knowledge_base_id = knowledge_base_id.clone();
            let doc = doc.clone();
            let options = options.clone();
            let cancel = cancel;
            let vault = vault;
            futures.push(async move {
                let mapping = execute_doc(
                    api,
                    vault,
                    &knowledge_base_id,
                    &doc,
                    existing.as_ref(),
                    &tag_ids,
                    &options,
                    cancel,
                )
                .await;
                (doc.record.path.clone(), mapping, tag_ids)
            });
        }
        let results = futures_util::future::join_all(futures).await;
        for (path, mapping, tag_ids) in results {
            done += 1;
            let created = plan.create.contains(&path);
            if mapping.status == STATUS_FAILED {
                report.failed += 1;
                errors.push(SyncError {
                    paper: path.clone(),
                    error: mapping
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "sync failed".into()),
                });
            } else if created {
                report.created += 1;
            } else {
                report.updated += 1;
            }
            if !mapping.knowledge_id.is_empty() && !tag_ids.is_empty() {
                tag_updates.insert(mapping.knowledge_id.clone(), tag_ids);
            }
            state.docs.insert(path.clone(), mapping);
            state.save(vault)?;
        }
        current.done = done;
        current.created = report.created;
        current.updated = report.updated;
        current.failed = report.failed;
        current.errors = errors.clone();
        current.current = batch.last().map(|path| (*path).clone());
        progress(current.clone());
    }

    // Batch tag assignment (one `PUT /knowledge/tags` per run).
    if !tag_updates.is_empty() {
        if let Err(error) = api
            .set_knowledge_tags(&knowledge_base_id, &tag_updates)
            .await
        {
            log::warn!("weknora: tag assignment failed: {error}");
        }
    }

    // Deletions: only documents this engine created for this Vault.
    if !plan.delete.is_empty() && !cancel.load(Ordering::SeqCst) {
        let mut ids = Vec::new();
        for path in &plan.delete {
            if let Some(doc) = state.docs.get(path) {
                if !doc.knowledge_id.is_empty() {
                    ids.push(doc.knowledge_id.clone());
                }
            }
        }
        for chunk in ids.chunks(200) {
            match api
                .batch_delete_knowledge(&knowledge_base_id, chunk)
                .await
            {
                Ok(deleted) => {
                    report.deleted += deleted;
                }
                Err(error) => {
                    report.failed += chunk.len();
                    errors.push(SyncError {
                        paper: "<deleted papers>".into(),
                        error: error.to_string(),
                    });
                }
            }
        }
        for path in &plan.delete {
            state.docs.remove(path);
        }
        state.save(vault)?;
    }

    state.last_sync_at = Some(now_rfc3339_millis());
    state.save(vault)?;
    report.errors = errors;
    current.phase = SyncPhase::Done;
    current.running = false;
    current.done = report.total;
    current.deleted = report.deleted;
    progress(current);
    Ok(report)
}

/// Rebuild the local mapping from WeKnora `custom_metadata`; optionally delete
/// managed documents whose paper disappeared from the Vault.
pub async fn reconcile(
    api: &dyn KnowledgeApi,
    vault: &Path,
    knowledge_base_id: &str,
    state: &mut WeKnoraSyncState,
    prune: bool,
) -> Result<ReconcileReport, AppError> {
    let catalog: HashSet<String> = papers::list_all(vault)?
        .into_iter()
        .map(|record| record.path)
        .collect();
    let vault_key = vault_key(vault);
    let mut managed: Vec<KnowledgeInfo> = Vec::new();
    for page in 1..=50u32 {
        let rows = api
            .list_knowledge(knowledge_base_id, page, 100)
            .await
            .map_err(AppError::from)?;
        let fetched = rows.len();
        managed.extend(rows.into_iter().filter(|info| {
            info.custom_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("agentero_managed"))
                .and_then(Value::as_str)
                == Some("true")
                && info
                    .custom_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("agentero_vault"))
                    .and_then(Value::as_str)
                    .is_some_and(|key| key == vault_key)
        }));
        if fetched < 100 {
            break;
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut report = ReconcileReport::default();
    for info in managed {
        seen.insert(info.id.clone());
        let path = info
            .custom_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("agentero_path"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !path.is_empty() && catalog.contains(&path) {
            let existing = state.docs.get(&path).cloned();
            let hash = info
                .custom_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("agentero_hash"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let status = if info.is_failed() {
                STATUS_FAILED
            } else if info.is_indexed() {
                STATUS_READY
            } else {
                STATUS_INDEXING
            };
            state.docs.insert(
                path.clone(),
                WeKnoraDocState {
                    knowledge_id: info.id.clone(),
                    content_hash: hash,
                    bibtex_key: existing
                        .as_ref()
                        .map(|doc| doc.bibtex_key.clone())
                        .unwrap_or_default(),
                    tag_ids: info.tags.iter().map(|tag| tag.id.clone()).collect(),
                    status: status.into(),
                    synced_at: existing.and_then(|doc| doc.synced_at),
                    last_error: if info.is_failed() {
                        info.parse_status.clone()
                    } else {
                        None
                    },
                },
            );
            report.adopted += 1;
        } else {
            report.orphans += 1;
            if prune {
                match api.delete_knowledge(&info.id).await {
                    Ok(()) => report.deleted += 1,
                    Err(error) => {
                        log::warn!("weknora reconcile: delete orphan {}: {error}", info.id);
                        report.failed += 1;
                    }
                }
            }
        }
    }
    // Stored docs whose knowledge id is no longer present are stale mappings.
    let before = state.docs.len();
    state
        .docs
        .retain(|_, doc| seen.contains(&doc.knowledge_id) || doc.knowledge_id.is_empty());
    report.missing = before.saturating_sub(state.docs.len());
    if state.kb_id != knowledge_base_id {
        state.prepare_for_kb(knowledge_base_id);
    }
    state.save(vault)?;
    Ok(report)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub adopted: usize,
    pub missing: usize,
    pub orphans: usize,
    pub deleted: usize,
    pub failed: usize,
}

/// Clear the local mapping; optionally delete every managed document of this
/// Vault from the knowledge base (dangerous, callers must confirm).
pub async fn clear_mapping(
    api: &dyn KnowledgeApi,
    vault: &Path,
    knowledge_base_id: &str,
    state: &mut WeKnoraSyncState,
    delete_docs: bool,
) -> Result<ClearMappingReport, AppError> {
    let mut deleted = 0usize;
    if delete_docs {
        let vault_key = vault_key(vault);
        let mut ids = Vec::new();
        for page in 1..=50u32 {
            let rows = api
                .list_knowledge(knowledge_base_id, page, 100)
                .await
                .map_err(AppError::from)?;
            let fetched = rows.len();
            ids.extend(rows.into_iter().filter_map(|info| {
                let metadata = info.custom_metadata.as_ref()?;
                let managed = metadata.get("agentero_managed").and_then(Value::as_str)
                    == Some("true");
                let same_vault = metadata
                    .get("agentero_vault")
                    .and_then(Value::as_str)
                    .is_some_and(|key| key == vault_key);
                (managed && same_vault).then_some(info.id)
            }));
            if fetched < 100 {
                break;
            }
        }
        for chunk in ids.chunks(200) {
            match api
                .batch_delete_knowledge(knowledge_base_id, chunk)
                .await
            {
                Ok(count) => deleted += count,
                Err(error) => log::warn!("weknora clear mapping: delete failed: {error}"),
            }
        }
    }
    let cleared = state.docs.len();
    state.docs.clear();
    state.last_sync_at = None;
    state.save(vault)?;
    Ok(ClearMappingReport { cleared, deleted })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClearMappingReport {
    pub cleared: usize,
    pub deleted: usize,
}

// ---------------------------------------------------------------------------
// helpers used by the web service
// ---------------------------------------------------------------------------

/// Count of papers that currently have a synced/indexing/failed document.
pub fn state_counts(state: &WeKnoraSyncState) -> state::WeKnoraStateCounts {
    state.counts()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::paper::catalog::papers::PaperKind;
    use crate::features::weknora::client::SearchRequest;
    use std::sync::Mutex;

    fn record(path: &str, title: &str) -> PaperRecord {
        let mut record = PaperRecord::local_pdf(
            path.rsplit('/').next().unwrap_or("demo").to_string(),
            title.into(),
        )
        .at_path(path);
        record.paper_type = PaperKind::Doi;
        record.authors = vec!["Ada Lovelace".into()];
        record.year = Some(2024);
        record
    }

    fn rendered_doc(path: &str, hash: &str) -> RenderedDoc {
        RenderedDoc {
            record: record(path, "Demo"),
            rendered: RenderOutput {
                markdown: "# Demo\n".into(),
                includes: Default::default(),
                truncated: false,
                bibtex_key: "lovelace2024".into(),
            },
            hash: hash.into(),
        }
    }

    #[test]
    fn diff_classifies_create_update_skip_delete() {
        let mut rendered = BTreeMap::new();
        rendered.insert("papers/new".into(), rendered_doc("papers/new", "h1"));
        rendered.insert("papers/changed".into(), rendered_doc("papers/changed", "h2"));
        rendered.insert("papers/same".into(), rendered_doc("papers/same", "h3"));

        let mut state = WeKnoraSyncState::default();
        state.prepare_for_kb("kb");
        state.docs.insert(
            "papers/changed".into(),
            WeKnoraDocState {
                knowledge_id: "doc-changed".into(),
                content_hash: "old".into(),
                bibtex_key: String::new(),
                tag_ids: Vec::new(),
                status: STATUS_READY.into(),
                synced_at: None,
                last_error: None,
            },
        );
        state.docs.insert(
            "papers/same".into(),
            WeKnoraDocState {
                knowledge_id: "doc-same".into(),
                content_hash: "h3".into(),
                bibtex_key: String::new(),
                tag_ids: Vec::new(),
                status: STATUS_READY.into(),
                synced_at: None,
                last_error: None,
            },
        );
        state.docs.insert(
            "papers/gone".into(),
            WeKnoraDocState {
                knowledge_id: "doc-gone".into(),
                content_hash: "h4".into(),
                bibtex_key: String::new(),
                tag_ids: Vec::new(),
                status: STATUS_READY.into(),
                synced_at: None,
                last_error: None,
            },
        );

        let plan = diff(&rendered, &state, false, true);
        assert_eq!(plan.create, vec!["papers/new"]);
        assert_eq!(plan.update, vec!["papers/changed"]);
        assert_eq!(plan.skip, vec!["papers/same"]);
        assert_eq!(plan.delete, vec!["papers/gone"]);

        let forced = diff(&rendered, &state, true, true);
        assert_eq!(forced.create, vec!["papers/new"], "force keeps new docs as create");
        assert_eq!(forced.update.len(), 2, "force re-updates existing docs");
        assert!(forced.skip.is_empty());

        let single = diff(&rendered, &state, false, false);
        assert!(single.delete.is_empty(), "single-paper runs never delete");
    }

    #[test]
    fn custom_metadata_has_the_documented_fields() {
        let vault = tempfile::tempdir().unwrap();
        let doc = rendered_doc("papers/demo", "hash-1");
        let metadata = custom_metadata(vault.path(), &doc);
        assert!(metadata.len() <= crate::features::weknora::client::MAX_CUSTOM_METADATA_FIELDS);
        for key in [
            "agentero_managed",
            "agentero_vault",
            "agentero_paper_id",
            "agentero_path",
            "agentero_title",
            "agentero_kind",
            "agentero_renderer",
            "agentero_hash",
            "agentero_source_updated_at",
            "agentero_has_body",
            "agentero_notes",
            "agentero_marks",
            "agentero_layout",
            "agentero_pdf_excluded",
        ] {
            assert!(metadata.contains_key(key), "missing {key}");
        }
        assert_eq!(metadata["agentero_managed"], "true");
        assert_eq!(metadata["agentero_pdf_excluded"], "true");
        assert_eq!(metadata["agentero_vault"], vault_key(vault.path()));
        let raw = serde_json::to_string(&metadata).unwrap();
        assert!(!raw.contains("api_key"), "{raw}");
    }

    /// In-memory WeKnora used by the engine tests.
    struct MockApi {
        inner: Mutex<MockInner>,
    }

    #[derive(Default)]
    struct MockInner {
        docs: BTreeMap<String, KnowledgeInfo>,
        contents: BTreeMap<String, String>,
        metadata: BTreeMap<String, Map<String, Value>>,
        tags: Vec<TagInfo>,
        tag_updates: Vec<HashMap<String, Vec<String>>>,
        deleted: Vec<String>,
        next_id: usize,
        index_ready: bool,
    }

    impl MockApi {
        fn new() -> Self {
            Self {
                inner: Mutex::new(MockInner {
                    index_ready: true,
                    ..Default::default()
                }),
            }
        }
        fn document_count(&self) -> usize {
            self.inner.lock().unwrap().docs.len()
        }
    }

    fn ok_info(id: &str) -> KnowledgeInfo {
        KnowledgeInfo {
            id: id.into(),
            title: Some("Demo".into()),
            parse_status: Some("completed".into()),
            enable_status: Some("enabled".into()),
            custom_metadata: None,
            tags: Vec::new(),
            updated_at: None,
        }
    }

    #[async_trait]
    impl KnowledgeApi for MockApi {
        async fn create_manual(
            &self,
            _kb: &str,
            _title: &str,
            content: &str,
            _tag_ids: &[String],
        ) -> Result<String, WeKnoraError> {
            let mut inner = self.inner.lock().unwrap();
            inner.next_id += 1;
            let id = format!("doc-{}", inner.next_id);
            inner.contents.insert(id.clone(), content.to_string());
            inner.docs.insert(id.clone(), ok_info(&id));
            Ok(id)
        }
        async fn update_manual(
            &self,
            knowledge_id: &str,
            _title: &str,
            content: &str,
            _tag_ids: &[String],
        ) -> Result<(), WeKnoraError> {
            let mut inner = self.inner.lock().unwrap();
            if !inner.docs.contains_key(knowledge_id) {
                return Err(WeKnoraError::new(
                    crate::features::weknora::client::WeKnoraErrorKind::NotFound,
                    "missing doc",
                ));
            }
            inner.contents.insert(knowledge_id.to_string(), content.to_string());
            Ok(())
        }
        async fn update_metadata(
            &self,
            knowledge_id: &str,
            _title: &str,
            custom_metadata: &Map<String, Value>,
        ) -> Result<(), WeKnoraError> {
            self.inner
                .lock()
                .unwrap()
                .metadata
                .insert(knowledge_id.to_string(), custom_metadata.clone());
            Ok(())
        }
        async fn get_knowledge(&self, knowledge_id: &str) -> Result<KnowledgeInfo, WeKnoraError> {
            let inner = self.inner.lock().unwrap();
            let mut info = inner
                .docs
                .get(knowledge_id)
                .cloned()
                .unwrap_or_else(|| ok_info(knowledge_id));
            if !inner.index_ready {
                info.parse_status = Some("processing".into());
                info.enable_status = Some("disabled".into());
            }
            Ok(info)
        }
        async fn delete_knowledge(&self, knowledge_id: &str) -> Result<(), WeKnoraError> {
            let mut inner = self.inner.lock().unwrap();
            inner.docs.remove(knowledge_id);
            inner.deleted.push(knowledge_id.into());
            Ok(())
        }
        async fn batch_delete_knowledge(
            &self,
            _kb: &str,
            ids: &[String],
        ) -> Result<usize, WeKnoraError> {
            let mut inner = self.inner.lock().unwrap();
            for id in ids {
                inner.docs.remove(id);
                inner.deleted.push(id.clone());
            }
            Ok(ids.len())
        }
        async fn list_tags(&self, _kb: &str) -> Result<Vec<TagInfo>, WeKnoraError> {
            Ok(self.inner.lock().unwrap().tags.clone())
        }
        async fn create_tag(&self, _kb: &str, name: &str) -> Result<TagInfo, WeKnoraError> {
            let mut inner = self.inner.lock().unwrap();
            let tag = TagInfo {
                id: format!("tag-{}", inner.tags.len() + 1),
                name: name.to_string(),
            };
            inner.tags.push(tag.clone());
            Ok(tag)
        }
        async fn set_knowledge_tags(
            &self,
            _kb: &str,
            updates: &HashMap<String, Vec<String>>,
        ) -> Result<(), WeKnoraError> {
            self.inner.lock().unwrap().tag_updates.push(updates.clone());
            Ok(())
        }
        async fn list_knowledge(
            &self,
            _kb: &str,
            page: u32,
            _page_size: u32,
        ) -> Result<Vec<KnowledgeInfo>, WeKnoraError> {
            if page > 1 {
                return Ok(Vec::new());
            }
            Ok(self.inner.lock().unwrap().docs.values().cloned().collect())
        }
        async fn search(&self, _request: &SearchRequest) -> Result<Vec<SearchHit>, WeKnoraError> {
            Ok(Vec::new())
        }
    }

    fn settings() -> WeKnoraSettings {
        WeKnoraSettings {
            enabled: true,
            base_url: "https://kb.example.com".into(),
            knowledge_base_id: "kb-1".into(),
            ..Default::default()
        }
    }

    fn fixture_vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, title) in [("alpha", "Alpha"), ("beta", "Beta")] {
            let paper = dir.path().join("papers").join(name);
            std::fs::create_dir_all(&paper).unwrap();
            std::fs::write(paper.join("PAPER.md"), format!("# {title}\n\nBody.\n")).unwrap();
            std::fs::write(
                paper.join("NOTES.md"),
                format!("# Notes {title}\n\nSee [[{name}]].\n"),
            )
            .unwrap();
            let mut row =
                PaperRecord::local_pdf(name.to_string(), title.into()).at_path(&format!("papers/{name}"));
            row.paper_type = PaperKind::Doi;
            row.authors = vec!["Ada Lovelace".into()];
            row.year = Some(2024);
            row.tags = vec![crate::features::paper::catalog::papers::PaperTag::new("GaN")];
            papers::upsert_paper(dir.path(), &row).unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn full_sync_creates_then_skips_then_updates_and_deletes() {
        let vault = fixture_vault();
        let api = MockApi::new();
        let mut state = WeKnoraSyncState::default();
        let settings = settings();
        let mount = BibtexMountSettings::default();
        let cancel = AtomicBool::new(false);
        let progress = |_progress: SyncProgress| {};

        let report = run(
            &api,
            vault.path(),
            &settings,
            &mount,
            &mut state,
            SyncOptions::from_settings(&settings, false, None),
            &cancel,
            &progress,
        )
        .await
        .unwrap();
        assert_eq!(report.created, 2);
        assert_eq!(report.total, 2);
        assert_eq!(api.document_count(), 2);
        assert!(!state.docs.is_empty());
        // The mock store receives managed documents with metadata.
        assert_eq!(state.docs.len(), 2);

        // Second run: nothing changed → both skipped.
        let report = run(
            &api,
            vault.path(),
            &settings,
            &mount,
            &mut state,
            SyncOptions::from_settings(&settings, false, None),
            &cancel,
            &progress,
        )
        .await
        .unwrap();
        assert_eq!(report.skipped, 2);
        assert_eq!(report.created, 0);
        assert_eq!(report.updated, 0);

        // Change one paper → one update, other skipped.
        std::fs::write(
            vault.path().join("papers/alpha/NOTES.md"),
            "# Notes Alpha\n\nnew note text\n",
        )
        .unwrap();
        let report = run(
            &api,
            vault.path(),
            &settings,
            &mount,
            &mut state,
            SyncOptions::from_settings(&settings, false, None),
            &cancel,
            &progress,
        )
        .await
        .unwrap();
        assert_eq!(report.updated, 1, "{report:?}");
        assert_eq!(report.skipped, 1);

        // Remove a paper from the catalog + disk → next run deletes its doc.
        papers::delete_under_path(vault.path(), "papers/beta").unwrap();
        std::fs::remove_dir_all(vault.path().join("papers/beta")).unwrap();
        let report = run(
            &api,
            vault.path(),
            &settings,
            &mount,
            &mut state,
            SyncOptions::from_settings(&settings, false, None),
            &cancel,
            &progress,
        )
        .await
        .unwrap();
        assert_eq!(report.deleted, 1, "{report:?}");
        assert_eq!(api.document_count(), 1);

        // Tags were created once and assigned.
        assert!(
            state
                .docs
                .values()
                .all(|doc| doc.tag_ids == vec!["tag-1".to_string()]),
            "{:?}",
            state.docs
        );
    }

    #[tokio::test]
    async fn index_timeout_marks_document_as_indexing() {
        let vault = fixture_vault();
        let api = MockApi::new();
        // Make the doc never leave `processing`.
        {
            let mut inner = api.inner.lock().unwrap();
            inner.index_ready = false;
        }
        let mut state = WeKnoraSyncState::default();
        let settings = settings();
        let mount = BibtexMountSettings::default();
        let mut options = SyncOptions::from_settings(&settings, false, None);
        options.index_timeout = Duration::from_millis(10);
        options.index_poll_interval = Duration::from_millis(5);
        let cancel = AtomicBool::new(false);
        let report = run(
            &api,
            vault.path(),
            &settings,
            &mount,
            &mut state,
            options,
            &cancel,
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(report.created, 2, "{report:?}");
        assert!(
            state
                .docs
                .values()
                .all(|doc| doc.status == STATUS_INDEXING),
            "{:?}",
            state.docs
        );
    }

    #[tokio::test]
    async fn cancellation_stops_before_creating_documents() {
        let vault = fixture_vault();
        let api = MockApi::new();
        let mut state = WeKnoraSyncState::default();
        let settings = settings();
        let mount = BibtexMountSettings::default();
        let cancel = AtomicBool::new(true);
        let report = run(
            &api,
            vault.path(),
            &settings,
            &mount,
            &mut state,
            SyncOptions::from_settings(&settings, false, None),
            &cancel,
            &|_| {},
        )
        .await
        .unwrap();
        assert!(report.cancelled);
        assert_eq!(api.document_count(), 0);
    }

    #[tokio::test]
    async fn reconcile_adopts_managed_docs_and_reports_orphans() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault.path().join("papers/alpha")).unwrap();
        papers::upsert_paper(
            vault.path(),
            &record("papers/alpha", "Alpha"),
        )
        .unwrap();
        let api = MockApi::new();
        let vault_scope = vault_key(vault.path());
        {
            let mut inner = api.inner.lock().unwrap();
            for (id, path) in [("doc-keep", "papers/alpha"), ("doc-orphan", "papers/gone")] {
                let mut metadata = Map::new();
                metadata.insert("agentero_managed".into(), Value::String("true".into()));
                metadata.insert(
                    "agentero_vault".into(),
                    Value::String(vault_scope.clone()),
                );
                metadata.insert("agentero_path".into(), Value::String(path.into()));
                metadata.insert("agentero_hash".into(), Value::String("hash".into()));
                inner.metadata.insert(id.to_string(), metadata);
                let mut info = ok_info(id);
                info.custom_metadata = inner.metadata.get(id).cloned();
                inner.docs.insert(id.to_string(), info);
            }
        }
        let mut state = WeKnoraSyncState::default();
        state.prepare_for_kb("kb-1");
        state.docs.insert(
            "papers/stale".into(),
            WeKnoraDocState {
                knowledge_id: "doc-stale".into(),
                content_hash: "h".into(),
                bibtex_key: String::new(),
                tag_ids: Vec::new(),
                status: STATUS_READY.into(),
                synced_at: None,
                last_error: None,
            },
        );
        let report = reconcile(&api, vault.path(), "kb-1", &mut state, false)
            .await
            .unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(report.orphans, 1);
        assert_eq!(report.missing, 1);
        assert!(state.docs.contains_key("papers/alpha"));
        assert!(!state.docs.contains_key("papers/stale"));

        // Prune deletes the orphan but never non-managed docs.
        let api = MockApi::new();
        {
            let mut inner = api.inner.lock().unwrap();
            let mut info = ok_info("doc-orphan");
            let mut metadata = Map::new();
            metadata.insert("agentero_managed".into(), Value::String("true".into()));
            metadata.insert(
                "agentero_vault".into(),
                Value::String(vault_key(vault.path())),
            );
            metadata.insert("agentero_path".into(), Value::String("papers/gone".into()));
            info.custom_metadata = Some(metadata);
            inner.docs.insert("doc-orphan".into(), info);
        }
        let report = reconcile(&api, vault.path(), "kb-1", &mut state, true)
            .await
            .unwrap();
        assert_eq!(report.deleted, 1);
        assert_eq!(api.document_count(), 0);
    }

    #[tokio::test]
    async fn clear_mapping_can_delete_managed_documents() {
        let vault = tempfile::tempdir().unwrap();
        let api = MockApi::new();
        {
            let mut inner = api.inner.lock().unwrap();
            let mut info = ok_info("doc-1");
            let mut metadata = Map::new();
            metadata.insert("agentero_managed".into(), Value::String("true".into()));
            metadata.insert(
                "agentero_vault".into(),
                Value::String(vault_key(vault.path())),
            );
            info.custom_metadata = Some(metadata);
            inner.docs.insert("doc-1".into(), info);
            let mut foreign = ok_info("doc-foreign");
            foreign.custom_metadata = Some(Map::new());
            inner.docs.insert("doc-foreign".into(), foreign);
        }
        let mut state = WeKnoraSyncState::default();
        state.docs.insert(
            "papers/a".into(),
            WeKnoraDocState {
                knowledge_id: "doc-1".into(),
                content_hash: "h".into(),
                bibtex_key: String::new(),
                tag_ids: Vec::new(),
                status: STATUS_READY.into(),
                synced_at: None,
                last_error: None,
            },
        );
        let report = clear_mapping(&api, vault.path(), "kb-1", &mut state, true)
            .await
            .unwrap();
        assert_eq!(report.cleared, 1);
        assert_eq!(report.deleted, 1);
        // The non-managed document is untouched.
        assert_eq!(api.document_count(), 1);
        assert!(state.docs.is_empty());
    }
}
