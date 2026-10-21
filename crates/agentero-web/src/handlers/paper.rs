//! Paper catalog handlers: list / detail / mutations / reference sidecars.
//!
//! Mirrors the desktop command shells under
//! `src-tauri/src/features/paper/{catalog,analyze/refs,import}/commands.rs`, but
//! calls the tauri-free `agentero-core` services directly. Wire shapes follow
//! `src/lib/core/bindings.ts` exactly (camelCase args, `ApiResult` envelope,
//! `Result<ApiResult<T>, String>` transport rejection for typed commands).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use agentero_core::features::paper::analyze::refs;
use agentero_core::features::paper::catalog::move_paper::move_with_index;
use agentero_core::features::paper::catalog::papers::{self, PaperRecord};
use agentero_core::features::paper::catalog::tag_index;
use agentero_core::features::paper::import::seed_notes_template;
use agentero_core::features::paper::venue;
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
