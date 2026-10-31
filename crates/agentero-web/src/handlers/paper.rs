//! Paper catalog handlers: list / detail / mutations / reference sidecars.
//!
//! Mirrors the desktop command shells under
//! `src-tauri/src/features/paper/{catalog,analyze/refs,import}/commands.rs`, but
//! calls the tauri-free `agentero-core` services directly. Wire shapes follow
//! `src/lib/core/bindings.ts` exactly (camelCase args, `ApiResult` envelope,
//! `Result<ApiResult<T>, String>` transport rejection for typed commands).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use agentero_core::features::paper::analyze::refs;
use agentero_core::features::paper::capabilities::CapsCache;
use agentero_core::features::paper::catalog::move_paper::move_with_index;
use agentero_core::features::paper::catalog::papers::{self, PaperRecord};
use agentero_core::features::paper::catalog::tag_index;
use agentero_core::features::paper::import::seed_notes_template;
use agentero_core::features::paper::venue;
use agentero_core::features::pdf::layout_index;
use agentero_core::features::pdf::marks::activity;
use agentero_core::fs::{resolve_paper_dir, sanitize_vault_rel};
use agentero_core::usage;

use crate::error::{
    blocking, fail_msg, ok, parse_args, typed_err, typed_ok, vault_root, HandlerResult,
};
use crate::handlers::wiki_state::lock_wiki_index;
use crate::rpc::RpcCtx;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperListArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperGetArgs {
    vault_path: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperPageCountsArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperReadingActivityBatchArgs {
    vault_path: String,
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperOpenBundleArgs {
    vault_path: String,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperSetTagsArgs {
    vault_path: String,
    path: String,
    tags: Vec<papers::PaperTag>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperSetIsReadArgs {
    vault_path: String,
    path: String,
    is_read: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperUpdateMetaArgs {
    vault_path: String,
    path: String,
    patch: papers::PaperMetaPatch,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperMoveArgs {
    vault_path: String,
    from_rel: String,
    dest_parent_rel: String,
    #[serde(default)]
    dirty_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperRepathArgs {
    vault_path: String,
    from_rel: String,
    to_rel: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperRescanArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperEnsureRowsArgs {
    vault_path: String,
    /// Absolute or vault-relative paths under `papers/` touched by the watcher.
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperSetPageCountsArgs {
    vault_path: String,
    counts: HashMap<String, i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperRefsSidecarArgs {
    vault_path: String,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperRefsParseArgs {
    vault_path: String,
    path: String,
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperSyncPublicationArgs {
    vault_path: String,
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotesTemplateSeedArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityRecordArgs {
    events: Vec<usage::UsageRecord>,
}

/// One library row: the catalog record flattened plus a local-PDF probe.
/// Wire-compatible with the desktop `PaperListRow` (`has_pdf` is snake_case).
#[derive(Debug, Serialize)]
struct PaperListRow {
    #[serde(flatten)]
    paper: PaperRecord,
    has_pdf: bool,
}

/// Bundle local paper-open data for the renderer's focus path (desktop
/// `PaperOpenBundle` shape).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaperOpenBundle {
    paper: PaperRecord,
    path_rel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes_seed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pdf_path: Option<String>,
    has_tex: bool,
    has_paper_md: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaperRescanResult {
    count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaperEnsureRowsResult {
    /// Rows inserted; existing rows are left as they are unless newer on disk.
    count: usize,
    /// True when a path could not be pinned to one paper folder and the
    /// command rebuilt the catalog from `papers/`.
    rescanned: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaperRepathResult {
    count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NotesTemplateSeedResult {
    created: bool,
}

/// Normalize + validate a command-supplied vault-relative paper path (same
/// rules as the desktop shared service): trim, strip slashes, `\` → `/`, reject
/// empty.
fn normalize_paper_path(path: &str) -> Result<String, String> {
    let path = path.trim().trim_matches('/').replace('\\', "/");
    if path.is_empty() {
        return Err("path is required".to_string());
    }
    Ok(path)
}

/// `paper_list({vaultPath})` → `Result<ApiResult<PaperListRow[]>, String>`.
pub async fn list(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperListArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let caps = ctx.state.caps.clone();
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let rows = papers::list_all_unique_by_id(&vault).map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(|paper| {
                let has_pdf =
                    !paper.path.is_empty() && caps.caps_for(&vault, &paper.path).has_pdf();
                PaperListRow { paper, has_pdf }
            })
            .collect::<Vec<_>>())
    })
    .await;
    match result {
        Ok(rows) => typed_ok(rows),
        Err(error) => typed_err(error),
    }
}

/// `paper_get({vaultPath, path?, id?})` → `ApiResult<PaperRecord>`.
pub async fn get(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperGetArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let record = match (args.path.as_deref(), args.id.as_deref()) {
            (Some(path), _) if !path.trim().is_empty() => {
                let path = path.trim().trim_matches('/').replace('\\', "/");
                papers::get_by_path(&vault, &path)
            }
            (_, Some(id)) if !id.trim().is_empty() => papers::get_by_id(&vault, id.trim()),
            _ => return Err("path or id is required".to_string()),
        }
        .map_err(|error| error.to_string())?;
        record.ok_or_else(|| "paper not found in catalog".to_string())
    })
    .await;
    match result {
        Ok(record) => ok(record),
        Err(error) => fail_msg(error),
    }
}

/// `paper_page_counts({vaultPath})` → `ApiResult<HashMap<String, i64>>`.
pub async fn page_counts(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperPageCountsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        papers::list_page_counts(&vault).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(counts) => ok(counts),
        Err(error) => fail_msg(error),
    }
}

/// `paper_reading_activity_batch({vaultPath, paths})` → activity heatmap points.
pub async fn reading_activity_batch(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperReadingActivityBatchArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let out: HashMap<String, Vec<activity::ReadingActivityPoint>> =
            activity::collect_reading_activity(&vault, &args.paths);
        Ok(out)
    })
    .await;
    match result {
        Ok(points) => ok(points),
        Err(error) => fail_msg(error),
    }
}

/// `paper_open_bundle({vaultPath, path})` → `ApiResult<PaperOpenBundle>`.
pub async fn open_bundle(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperOpenBundleArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let caps = ctx.state.caps.clone();
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let (_paper_dir, path_rel) =
            resolve_paper_dir(&vault, &args.path).map_err(|error| error.to_string())?;
        let paper = papers::get_by_path(&vault, &path_rel)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "paper not found in catalog".to_string())?;
        let paper_caps = caps.caps_for(&vault, &path_rel);
        let notes_seed = match std::fs::read_to_string(vault.join(&path_rel).join("NOTES.md")) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("read NOTES.md: {error}")),
        };
        Ok(PaperOpenBundle {
            paper,
            path_rel,
            notes_seed,
            pdf_path: paper_caps
                .pdf_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            has_tex: paper_caps.has_tex,
            has_paper_md: paper_caps.has_paper_md,
        })
    })
    .await;
    match result {
        Ok(bundle) => typed_ok(bundle),
        Err(error) => fail_msg(error),
    }
}

/// `paper_set_tags({vaultPath, path, tags})` → `ApiResult<PaperRecord>`.
pub async fn set_tags(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperSetTagsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let path = normalize_paper_path(&args.path)?;
        let record =
            papers::set_tags(&vault, &path, &args.tags).map_err(|error| error.to_string())?;
        // Keep the hidden tag index warm for the next sidebar/Library read.
        tag_index::invalidate_tag_index(&vault);
        tag_index::ensure_tag_index_best_effort(&vault);
        Ok(record)
    })
    .await;
    match result {
        Ok(record) => ok(record),
        Err(error) => fail_msg(error),
    }
}

/// `paper_set_is_read({vaultPath, path, isRead})` → `ApiResult<PaperRecord>`.
pub async fn set_is_read(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperSetIsReadArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let path = normalize_paper_path(&args.path)?;
        papers::set_is_read(&vault, &path, args.is_read).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(record) => ok(record),
        Err(error) => fail_msg(error),
    }
}

/// `paper_update_meta({vaultPath, path, patch})` → `ApiResult<PaperRecord>`.
pub async fn update_meta(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperUpdateMetaArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let path = normalize_paper_path(&args.path)?;
        let record =
            papers::update_meta(&vault, &path, &args.patch).map_err(|error| error.to_string())?;
        // A title/alias patch changes the tag index projection too.
        tag_index::invalidate_tag_index(&vault);
        tag_index::ensure_tag_index_best_effort(&vault);
        Ok(record)
    })
    .await;
    match result {
        Ok(record) => ok(record),
        Err(error) => fail_msg(error),
    }
}

/// `paper_move({vaultPath, fromRel, destParentRel, dirtyPaths?})` →
/// `ApiResult<PaperMoveResult>`.
pub async fn move_paper(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperMoveArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let mut guard = lock_wiki_index()?;
        let result = move_with_index(
            &vault,
            &args.from_rel,
            &args.dest_parent_rel,
            &args.dirty_paths,
            &mut guard,
        )
        .map_err(|error| error.to_string())?;
        // Paper paths moved → refresh the cached tag index.
        tag_index::invalidate_tag_index(&vault);
        tag_index::ensure_tag_index_best_effort(&vault);
        Ok(result)
    })
    .await;
    match result {
        Ok(result) => typed_ok(result),
        Err(error) => fail_msg(error),
    }
}

/// `paper_repath({vaultPath, fromRel, toRel})` → `ApiResult<PaperRepathResult>`.
pub async fn repath(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperRepathArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let count = papers::move_under_path(&vault, &args.from_rel, &args.to_rel)
            .map_err(|error| error.to_string())?;
        tag_index::invalidate_tag_index(&vault);
        tag_index::ensure_tag_index_best_effort(&vault);
        Ok(PaperRepathResult { count })
    })
    .await;
    match result {
        Ok(result) => ok(result),
        Err(error) => fail_msg(error),
    }
}

/// `paper_rescan({vaultPath})` → `ApiResult<PaperRescanResult>`.
pub async fn rescan(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperRescanArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let caps = ctx.state.caps.clone();
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let count = papers::rebuild_from_disk(&vault).map_err(|error| error.to_string())?;
        tag_index::invalidate_tag_index(&vault);
        tag_index::ensure_tag_index_best_effort(&vault);
        // A rescan follows file-level changes the catalog cannot see (e.g. a
        // PDF uploaded/overwritten from the browser), so the per-paper caps
        // cache must be dropped or `paper_list.has_pdf` stays stale.
        caps.clear();
        Ok(PaperRescanResult { count })
    })
    .await;
    match result {
        Ok(result) => ok(result),
        Err(error) => fail_msg(error),
    }
}

/// `paper_ensure_rows({vaultPath, paths})` → `ApiResult<PaperEnsureRowsResult>`.
///
/// Targeted catalog repair for FS-watcher paths: each path maps to at most one
/// paper folder, so an external create/replace costs one row instead of a full
/// `papers/` walk. `paper_rescan` stays the explicit whole-library rebuild; only
/// a path that is not one paper folder (the `papers/` root, an organization
/// folder, a delete outside a paper) falls back to it.
pub async fn ensure_rows(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperEnsureRowsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let touch = papers::ensure_rows_for_paths(&vault, &args.paths)
            .map_err(|error| error.to_string())?;
        // Only touched rows can change the tag projection; the fingerprinted
        // rebuild inside the tag index makes this a no-op when nothing changed.
        tag_index::ensure_tag_index_best_effort(&vault);
        Ok(match touch {
            papers::PaperRowsTouch::Ensured { count } => PaperEnsureRowsResult {
                count,
                rescanned: false,
            },
            papers::PaperRowsTouch::Rescanned { count } => PaperEnsureRowsResult {
                count,
                rescanned: true,
            },
        })
    })
    .await;
    match result {
        Ok(result) => ok(result),
        Err(error) => fail_msg(error),
    }
}

/// `paper_set_page_counts({vaultPath, counts})` → `ApiResult<null>`.
pub async fn set_page_counts(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperSetPageCountsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let counts: Vec<(String, i64)> = args
            .counts
            .into_iter()
            .map(|(path, count)| (path.trim().trim_matches('/').replace('\\', "/"), count))
            .filter(|(path, count)| !path.is_empty() && *count > 0)
            .collect();
        papers::set_page_counts(&vault, &counts).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(()) => ok(()),
        Err(error) => fail_msg(error),
    }
}

/// `paper_refs_list({vaultPath, path})` → `ApiResult<CiteSidecar | null>`.
pub async fn refs_list(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperRefsSidecarArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let rel = sanitize_vault_rel(&args.path).map_err(|_| "invalid paper path".to_string())?;
        let sidecar_path = vault.join(rel).join("source").join(refs::SIDECAR_FILE);
        Ok(refs::read_sidecar(&sidecar_path))
    })
    .await;
    match result {
        Ok(sidecar) => ok(sidecar),
        Err(error) => fail_msg(error),
    }
}

/// `paper_refs_parse({vaultPath, path, force?})` → `ApiResult<CiteSidecar>`.
pub async fn refs_parse(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperRefsParseArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let vault = match vault_root(&args.vault_path) {
        Ok(vault) => vault,
        Err(error) => return fail_msg(error),
    };
    match refs::parse_paper_refs(&vault, &args.path, true, args.force).await {
        Ok(sidecar) => typed_ok(sidecar),
        Err(error) => fail_msg(error.to_string()),
    }
}

/// `paper_sync_publication({vaultPath, paths})` → `ApiResult<VenueSyncReport>`.
pub async fn sync_publication(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperSyncPublicationArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let vault = match vault_root(&args.vault_path) {
        Ok(vault) => vault,
        Err(error) => return fail_msg(error),
    };
    let key = ctx.state.settings.easy_scholar_key();
    match venue::sync_publication(&vault, &args.paths, key.as_deref()).await {
        Ok(report) => ok(report),
        Err(error) => fail_msg(error.to_string()),
    }
}

/// `notes_template_seed(vaultPath)` → `ApiResult<NotesTemplateSeedResult>`.
pub async fn notes_template_seed(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: NotesTemplateSeedArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let created = seed_notes_template(&vault).map_err(|error| error.to_string())?;
        Ok(NotesTemplateSeedResult { created })
    })
    .await;
    match result {
        Ok(result) => ok(result),
        Err(error) => fail_msg(error),
    }
}

/// `activity_record_events({events})` → `ApiResult<number>` (XDG usage log).
pub async fn activity_record_events(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ActivityRecordArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let result =
        blocking(move || usage::record_default(&args.events).map_err(|error| error.to_string()))
            .await;
    match result {
        Ok(count) => typed_ok(count),
        Err(error) => fail_msg(error),
    }
}

// ---------------------------------------------------------------------------
// Library row assets: status badges + delete-from-context-menu
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperAssetsStatusArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperDeleteAssetsArgs {
    vault_path: String,
    /// Vault-relative paper folder path.
    paper_path: String,
    /// `pdf` (top-level PDFs) or `layout` (generated layout sidecars).
    kind: String,
}

/// One library row's generated-asset presence (`paper_assets_status`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaperAssetStatus {
    /// Vault-relative paper folder path.
    path: String,
    has_pdf: bool,
    has_layout: bool,
    has_layout_index: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaperDeleteAssetsResult {
    /// Vault-relative paths that were actually removed.
    removed: Vec<String>,
}

/// Status rows for every catalog paper (deduped by logical id, same set the
/// Library table shows).
fn collect_assets_status(vault: &Path, caps: &CapsCache) -> Result<Vec<PaperAssetStatus>, String> {
    let rows = papers::list_all_unique_by_id(vault).map_err(|error| error.to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for paper in rows {
        if paper.path.trim().is_empty() {
            continue;
        }
        let paper_caps = caps.caps_for(vault, &paper.path);
        let source = vault.join(&paper.path).join("source");
        out.push(PaperAssetStatus {
            has_pdf: paper_caps.has_pdf(),
            has_layout: source.join(layout_index::LAYOUT_RAW_FILE).is_file(),
            has_layout_index: source.join(layout_index::LAYOUT_INDEX_FILE).is_file(),
            path: paper.path,
        });
    }
    out.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(out)
}

fn paper_source_dir(paper_dir: &Path) -> PathBuf {
    paper_dir.join("source")
}

/// Delete one kind of generated asset for a paper folder and report what was
/// removed. Missing files are a successful no-op (empty `removed`).
fn delete_paper_assets(vault: &Path, paper_path: &str, kind: &str) -> Result<Vec<String>, String> {
    let rel = sanitize_vault_rel(paper_path).map_err(|_| "invalid paper path".to_string())?;
    let paper_dir = vault.join(&rel);
    if !paper_dir.is_dir() {
        return Err(format!("paper folder not found: {rel}"));
    }
    let mut removed = Vec::new();
    match kind.trim().to_ascii_lowercase().as_str() {
        "pdf" => {
            // Only the paper's own top-level PDFs (`{paperDir}/*.pdf`): figure
            // PDFs under `source/assets/**` belong to the parsed body and must
            // survive a re-download of the source PDF.
            let entries = std::fs::read_dir(&paper_dir)
                .map_err(|error| format!("read paper dir {rel}: {error}"))?;
            for entry in entries.flatten() {
                let path = entry.path();
                let is_file = entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false);
                let is_pdf = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
                if !is_file || !is_pdf {
                    continue;
                }
                if std::fs::remove_file(&path).is_ok() {
                    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                        removed.push(format!("{rel}/{name}"));
                    }
                }
            }
        }
        "layout" => {
            let source = paper_source_dir(&paper_dir);
            for name in [
                layout_index::LAYOUT_RAW_FILE,
                layout_index::LAYOUT_INDEX_FILE,
            ] {
                let target = source.join(name);
                if target.is_file() && std::fs::remove_file(&target).is_ok() {
                    removed.push(format!("{rel}/source/{name}"));
                }
            }
        }
        other => {
            return Err(format!(
                "unknown asset kind: {other} (expected \"pdf\" or \"layout\")"
            ))
        }
    }
    removed.sort();
    Ok(removed)
}

/// `paper_assets_status({vaultPath})` → `ApiResult<PaperAssetStatus[]>`.
pub async fn assets_status(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperAssetsStatusArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let caps = ctx.state.caps.clone();
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        collect_assets_status(&vault, &caps)
    })
    .await;
    match result {
        Ok(rows) => ok(rows),
        Err(error) => fail_msg(error),
    }
}

/// `paper_delete_assets({vaultPath, paperPath, kind})` →
/// `ApiResult<{removed: string[]}>`.
pub async fn delete_assets(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperDeleteAssetsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let caps = ctx.state.caps.clone();
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let removed = delete_paper_assets(&vault, &args.paper_path, &args.kind)?;
        // The caps probe and the tag index cache both mirror disk state.
        caps.invalidate(&vault, &args.paper_path);
        tag_index::invalidate_tag_index(&vault);
        tag_index::ensure_tag_index_best_effort(&vault);
        Ok(PaperDeleteAssetsResult { removed })
    })
    .await;
    match result {
        Ok(result) => ok(result),
        Err(error) => fail_msg(error),
    }
}

#[cfg(test)]
mod asset_tests {
    use super::*;
    use agentero_core::features::paper::catalog::papers::PaperRecord;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );
        let root = std::env::temp_dir().join(format!("agentero-web-assets-{unique}"));
        let paper_dir = root.join("papers/demo");
        std::fs::create_dir_all(paper_dir.join("source/assets")).unwrap();
        std::fs::write(paper_dir.join("demo.pdf"), b"%PDF-1.7").unwrap();
        std::fs::write(paper_dir.join("demo-extra.PDF"), b"%PDF-1.7").unwrap();
        std::fs::write(paper_dir.join("NOTES.md"), b"# notes").unwrap();
        std::fs::write(paper_dir.join("source/assets/figure.pdf"), b"%PDF-1.7").unwrap();
        std::fs::write(paper_dir.join("source/layout.json"), b"{}").unwrap();
        std::fs::write(paper_dir.join("source/layout-index.json"), b"{}").unwrap();
        papers::upsert_paper(
            &root,
            &PaperRecord::local_pdf("demo".into(), "Demo Paper".into()).at_path("papers/demo"),
        )
        .unwrap();
        root
    }

    #[test]
    fn reports_pdf_and_layout_presence() {
        let root = fixture();
        let caps = CapsCache::new();
        let rows = collect_assets_status(&root, &caps).expect("status");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "papers/demo");
        assert!(rows[0].has_pdf);
        assert!(rows[0].has_layout);
        assert!(rows[0].has_layout_index);

        // Layout-only deletion flips the generated-asset flags, not hasPdf.
        delete_paper_assets(&root, "papers/demo", "layout").expect("delete layout");
        let rows = collect_assets_status(&root, &caps).expect("status");
        assert!(rows[0].has_pdf);
        assert!(!rows[0].has_layout);
        assert!(!rows[0].has_layout_index);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn deletes_only_top_level_pdfs_and_layout_sidecars() {
        let root = fixture();
        let removed = delete_paper_assets(&root, "papers/demo", "pdf").expect("delete pdf");
        assert_eq!(
            removed,
            vec![
                "papers/demo/demo-extra.PDF".to_string(),
                "papers/demo/demo.pdf".to_string(),
            ]
        );
        assert!(!root.join("papers/demo/demo.pdf").exists());
        assert!(
            root.join("papers/demo/source/assets/figure.pdf").is_file(),
            "figure PDFs under source/assets must survive"
        );
        assert!(root.join("papers/demo/NOTES.md").is_file());

        let removed = delete_paper_assets(&root, "papers/demo", "layout").expect("delete layout");
        assert_eq!(
            removed,
            vec![
                "papers/demo/source/layout-index.json".to_string(),
                "papers/demo/source/layout.json".to_string(),
            ]
        );
        // Idempotent: nothing left to remove is still Ok with an empty list.
        let again = delete_paper_assets(&root, "papers/demo", "layout").expect("no-op");
        assert!(again.is_empty());
        let again = delete_paper_assets(&root, "papers/demo", "pdf").expect("no-op");
        assert!(again.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unknown_kind_and_missing_paper() {
        let root = fixture();
        assert!(delete_paper_assets(&root, "papers/demo", "video").is_err());
        assert!(delete_paper_assets(&root, "papers/missing", "pdf").is_err());
        assert!(delete_paper_assets(&root, "../escape", "pdf").is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
