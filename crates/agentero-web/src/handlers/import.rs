//! Import-domain handlers: local PDF import, identifier lookup, bibliography
//! import/export, skill installation.
//!
//! Every command here is a thin shell over the tauri-free
//! `agentero_core::features::paper::import` pipelines, so the browser gets the
//! same commit / dedupe / recognition semantics as the desktop Host and the
//! `agentero import` CLI:
//!
//! * local PDFs arrive as vault-internal absolute paths produced by
//!   `POST /api/upload` (or `paper_stage_import_file` for base64 drops);
//! * identifier lookups go through `lookup_import_batch` /
//!   `paper_resolve_identifier` (Translator + direct-connect fallbacks);
//! * `paper:imported` / `paper:assets-ready` reach the EventBus through the
//!   core `HostHooks` wired in `AppState::host_handle`.
//!
//! Server-specific deviation: the desktop defers metadata recognition for
//! metadata-less local PDFs to a `RecognizeMetadata` JobCenter runner, which the
//! headless host does not have in v1. The handler therefore asks core to run
//! recognition synchronously (`recognize_sync`) so the committed row carries
//! the real title / identifiers instead of the filename placeholder; the
//! deferred runner can replace this once it is ported.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use agentero_core::features::paper::import::recognize::pdf_recognize::recognize_and_resolve;
use agentero_core::features::paper::import::sources::zotero::{
    export_catalog, import_catalog_with_mode, PaperExportArgs,
};
use agentero_core::features::paper::import::{
    discard_skill_discovery, enrich_remote_urls, import_by_identifier_batch, import_local_pdfs,
    install_discovered_skills, resolve_metadata, stage_import_file, ImportLocalPdfArgs,
    ImportLocalPdfResult, LocalPdfExtraMeta, LocalPdfImportEntry, LookupImportBatchArgs,
    NoteShellMode, PaperImportArgs, StageImportFileArgs, DEFAULT_TRANSLATOR_BASE_URL,
};

use crate::error::{blocking, fail, fail_msg, ok, parse_args, typed_err, typed_ok, HandlerResult};
use crate::rpc::RpcCtx;

/// Per-PDF metadata-recognition budget. Recognition never fails the import:
/// on timeout / probe error the entry keeps filename-derived metadata and the
/// paper is committed anyway (the reason is surfaced in `assetMessages`).
const RECOGNIZE_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn dispatch(ctx: &RpcCtx, command: &str, args: &Value) -> Option<HandlerResult> {
    Some(match command {
        "paper_import_local_pdf" => paper_import_local_pdf(ctx, args).await,
        "paper_import" => paper_import(ctx, args).await,
        "paper_resolve_identifier" => paper_resolve_identifier(ctx, args).await,
        "lookup_import_batch" => lookup_import_batch(ctx, args).await,
        "paper_stage_import_file" => paper_stage_import_file(ctx, args).await,
        "paper_export" => paper_export(ctx, args).await,
        "skill_install" => skill_install(ctx, args).await,
        "skill_discard" => skill_discard(ctx, args).await,
        // papers.cool lives in the desktop discovery module (not in core yet).
        "paper_coolpapers_notes" | "paper_coolpapers_import" => fail_msg(
            "papers.cool import is not available in the headless web host",
        ),
        _ => return None,
    })
}

/// NOTES.md shell mode from the shared XDG settings (`paperNoteMode`).
pub(crate) fn note_mode(ctx: &RpcCtx) -> NoteShellMode {
    ctx.state
        .settings
        .get()
        .map(|snapshot| NoteShellMode::parse(&snapshot.settings.paper_note_mode))
        .unwrap_or(NoteShellMode::Standard)
}

/// Translator base URL: explicit override wins, else the settings value, else
/// the hosted default (mirrors `translator_base` in the desktop commands).
pub(crate) fn translator_base(ctx: &RpcCtx, override_url: Option<&str>) -> String {
    let from_override = override_url.map(str::trim).filter(|value| !value.is_empty());
    if let Some(value) = from_override {
        return value.trim_end_matches('/').to_string();
    }
    ctx.state
        .settings
        .get()
        .ok()
        .map(|snapshot| snapshot.settings.translator_base_url.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(|| DEFAULT_TRANSLATOR_BASE_URL.to_string())
}

/// `paper_import_local_pdf(entries | filePaths)` → `Result<ApiResult<ImportLocalPdfResult>, String>`.
///
/// The browser uploads file bytes through `POST /api/upload` first and passes
/// the resulting absolute path here; core copies the PDF into
/// `{parentDir}/{slug}/`, writes the shell/catalog row and emits
/// `paper:imported`.
///
/// The headless host has no deferred `RecognizeMetadata` job runner, so
/// metadata-less entries get a bounded synchronous recognition pass here
/// (`recognize_and_resolve`: liteparse probe → Zotero recognizer → identifier
/// resolution). Any failure or timeout falls back to filename metadata and the
/// reason is attached to the committed row instead of failing the import.
async fn paper_import_local_pdf(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ImportLocalPdfArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let ImportLocalPdfArgs {
        vault_path,
        parent_dir,
        file_paths,
        entries,
        task_id,
        recognize_sync: _,
        translator_base_url,
    } = args;
    // Bracket the staged writes (PDF copy, shell, catalog, NOTES) so the
    // watcher emits one merged `vault:file-changed` at the end instead of one
    // refresh per written file.
    let _write_burst = ctx.state.events.write_burst().guard();

    // `entries` (dialog metadata) win; picker-style absolute paths are converted
    // to bare entries — same precedence as `import_local_pdfs`.
    let pending: Vec<LocalPdfImportEntry> = if !entries.is_empty() {
        entries
    } else {
        file_paths
            .into_iter()
            .map(|file_path| LocalPdfImportEntry {
                file_path,
                title: None,
                authors: None,
                year: None,
                doi: None,
                arxiv_id: None,
                extra: None,
            })
            .collect()
    };
    let pending = dedupe_local_entries(pending);
    if pending.is_empty() {
        return typed_ok(ImportLocalPdfResult {
            papers: Vec::new(),
            errors: Vec::new(),
        });
    }

    let translator = translator_base(ctx, translator_base_url.as_deref());
    let note_mode = note_mode(ctx);
    let app = ctx.state.host_handle();
    let caps = ctx.state.caps.clone();
    let mut papers = Vec::new();
    let mut errors = Vec::new();

    // One file per call so a recognition fallback can be attached to exactly
    // that file's result row (`assetMessages`) — core still owns commit,
    // dedupe, catalog writes and `paper:imported`.
    for mut entry in pending {
        let fallback_reason = if entry_has_metadata(&entry) {
            None
        } else {
            recognize_entry(&mut entry, &translator).await
        };
        let single = ImportLocalPdfArgs {
            vault_path: vault_path.clone(),
            parent_dir: parent_dir.clone(),
            file_paths: Vec::new(),
            entries: vec![entry],
            task_id: task_id.clone(),
            recognize_sync: false,
            translator_base_url: Some(translator.clone()),
        };
        match import_local_pdfs(single, Some(&app), Some(&caps), note_mode).await {
            Ok(mut result) => {
                if let Some(reason) = &fallback_reason {
                    for paper in &mut result.papers {
                        paper
                            .asset_messages
                            .push(format!("metadata recognition: {reason}"));
                    }
                }
                papers.extend(result.papers);
                errors.extend(result.errors);
            }
            Err(error) => errors.push(error.to_string()),
        }
    }

    // Same contract as `import_local_pdfs`: total failure is an error, partial
    // success returns per-file errors alongside the imported rows.
    if papers.is_empty() && !errors.is_empty() {
        return fail_msg(errors.join("; "));
    }
    log::info!(
        "paper_import_local_pdf: imported={} errors={}",
        papers.len(),
        errors.len()
    );
    typed_ok(ImportLocalPdfResult { papers, errors })
}

fn entry_has_metadata(entry: &LocalPdfImportEntry) -> bool {
    entry.title.is_some()
        || entry.doi.is_some()
        || entry.arxiv_id.is_some()
        || entry.extra.is_some()
}

fn file_label(entry: &LocalPdfImportEntry) -> String {
    let raw = entry.file_path.trim();
    Path::new(raw)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| raw.to_string())
}

/// Bounded metadata recognition for one metadata-less local PDF. Fills the
/// entry with whatever was resolved (title/authors/year/identifiers/venue
/// fields) and returns the fallback reason when the row keeps partial or
/// filename metadata.
async fn recognize_entry(entry: &mut LocalPdfImportEntry, translator: &str) -> Option<String> {
    let raw = entry.file_path.trim().to_string();
    if raw.is_empty() || !Path::new(&raw).is_file() {
        // Let core report the real file problem ("file not found", "not a PDF").
        return None;
    }
    let label = file_label(entry);
    let probe = match tokio::time::timeout(
        RECOGNIZE_TIMEOUT,
        recognize_and_resolve(Path::new(&raw), translator, None),
    )
    .await
    {
        Ok(probe) => probe,
        Err(_) => {
            return Some(format!(
                "timed out after {}s; imported with filename metadata",
                RECOGNIZE_TIMEOUT.as_secs()
            ))
        }
    };

    if probe.status == "ok" || probe.status == "title" {
        if let Some(title) = probe
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            entry.title = Some(title.to_string());
        }
        if !probe.authors.is_empty() {
            entry.authors = Some(probe.authors.clone());
        }
        if probe.year.is_some() {
            entry.year = probe.year;
        }
        if let Some(doi) = probe.doi.as_deref().map(str::trim).filter(|doi| !doi.is_empty()) {
            entry.doi = Some(doi.to_string());
        }
        if let Some(arxiv) = probe
            .arxiv_id
            .as_deref()
            .map(str::trim)
            .filter(|arxiv| !arxiv.is_empty())
        {
            entry.arxiv_id = Some(arxiv.to_string());
        }
        entry.extra = Some(LocalPdfExtraMeta {
            publication: probe.publication.clone(),
            volume: probe.volume.clone(),
            issue: probe.issue.clone(),
            pages: probe.pages.clone(),
            publisher: probe.publisher.clone(),
            issn: None,
            language: None,
            date: None,
            abstract_text: probe.abstract_text.clone(),
        });
    }

    if probe.status == "ok" {
        None
    } else if probe.status == "title" {
        Some(format!(
            "no identifier matched for {label}; imported with recognized title"
        ))
    } else {
        let reason = probe
            .error
            .filter(|error| !error.trim().is_empty())
            .or_else(|| probe.warning.clone())
            .unwrap_or_else(|| format!("recognition failed for {label}"));
        Some(format!("{reason}; imported with filename metadata"))
    }
}

/// Drop repeated source files (same path spelled with `\` vs `/`) so one
/// upload/pick never commits the same PDF twice.
fn dedupe_local_entries(entries: Vec<LocalPdfImportEntry>) -> Vec<LocalPdfImportEntry> {
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter(|entry| {
            let mut key = entry.file_path.trim().replace('\\', "/");
            if cfg!(windows) {
                key = key.to_lowercase();
            }
            seen.insert(key)
        })
        .collect()
}

/// `paper_import({vaultPath, parentDir?, content})` → `Result<ApiResult<PaperImportResult>, String>`.
///
/// Bibliography-file import (BibTeX / RIS / …) through Translator `/import`;
/// the browser reads the picked file bytes and sends them as `content`.
async fn paper_import(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperImportArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let _write_burst = ctx.state.events.write_burst().guard();
    let note_mode = note_mode(ctx);
    let app = ctx.state.host_handle();
    match import_catalog_with_mode(args, Some(&app), note_mode).await {
        Ok(result) => typed_ok(result),
        Err(error) => fail(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveIdentifierArgs {
    /// DOI / arXiv id / URL / title text.
    text: String,
    #[serde(default)]
    translator_base_url: Option<String>,
}

/// `paper_resolve_identifier({text})` → `ApiResult<PaperRecord>` (plain envelope).
///
/// Resolves an identifier to catalog metadata without importing — backs Edit
/// Metadata's identifier refresh. Identifier resolution goes through the
/// Translator with the direct-connect fallback chains (arXiv/Crossref/S2/…).
async fn paper_resolve_identifier(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ResolveIdentifierArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let text = args.text.trim().to_string();
    if text.is_empty() {
        return fail_msg("text is required");
    }
    let base = translator_base(ctx, args.translator_base_url.as_deref());
    match resolve_metadata(&text, &base, None).await {
        Ok((mut meta, _used_translator)) => {
            enrich_remote_urls(&mut meta);
            ok(meta)
        }
        Err(error) => fail(error),
    }
}

/// `lookup_import_batch({vaultPath, parentDir, texts})` → `Result<ApiResult<LookupImportBatchResult>, String>`.
///
/// Batch identifier import used by the magic wand: dedupes within the batch and
/// against the catalog, resolves title-search candidates, and installs
/// discovered skills when the input is a skill source.
async fn lookup_import_batch(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: LookupImportBatchArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let _write_burst = ctx.state.events.write_burst().guard();
    let note_mode = note_mode(ctx);
    let app = ctx.state.host_handle();
    let caps = ctx.state.caps.clone();
    match import_by_identifier_batch(args, Some(&app), Some(&caps), note_mode).await {
        Ok(result) => typed_ok(result),
        Err(error) => fail(error),
    }
}

/// `paper_stage_import_file({fileName, contentBase64})` → `ApiResult<StageImportFileResult>`.
///
/// Fallback for path-less drops: writes the bytes to the server's
/// `~/.agentero/import-tmp/` and returns the absolute path for
/// `paper_import_local_pdf`. Browser uploads normally use `/api/upload` instead
/// (streamed body, no base64 inflation).
async fn paper_stage_import_file(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: StageImportFileArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    match blocking(move || stage_import_file(args).map_err(|error| error.to_string())).await {
        Ok(result) => ok(result),
        Err(error) => fail_msg(error),
    }
}

/// `paper_export({vaultPath, format?})` → `ApiResult<PaperExportResult>`.
///
/// Catalog → Zotero JSON array → Translator `/export` → bibliography text. The
/// browser turns the returned `content` into a download; no server-side save
/// dialog exists.
async fn paper_export(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperExportArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    match export_catalog(args).await {
        Ok(result) => ok(result),
        Err(error) => fail(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillInstallArgs {
    vault_path: String,
    discovery_id: String,
    #[serde(default)]
    selected_names: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    task_id: Option<String>,
}

/// `skill_install({vaultPath, discoveryId, selectedNames})` → `ApiResult<Vec<SkillImportResult>>`.
async fn skill_install(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SkillInstallArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    // A skill install materializes a whole directory tree; hold refresh frames
    // until it settles (one merged frame instead of a per-file storm).
    let _write_burst = ctx.state.events.write_burst().guard();
    let vault = match crate::error::vault_root(&args.vault_path) {
        Ok(vault) => vault,
        Err(error) => return fail_msg(error),
    };
    match install_discovered_skills(&vault, &args.discovery_id, &args.selected_names).await {
        Ok(result) => ok(result),
        Err(error) => fail(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillDiscardArgs {
    discovery_id: String,
}

/// `skill_discard(discoveryId)` → `ApiResult<null>`.
async fn skill_discard(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SkillDiscardArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let disclosure = args.discovery_id;
    match blocking(move || discard_skill_discovery(&disclosure).map_err(|error| error.to_string()))
        .await
    {
        Ok(()) => ok(()),
        Err(error) => fail_msg(error),
    }
}

