//! Server-side task center handlers and host-run job enqueues.
//!
//! Wire contract (`src/lib/core/bindings.ts`):
//! * `job_list` / `job_report` / `job_import_enqueue` / `job_download_assets_enqueue` /
//!   `job_parse_body_enqueue` / `job_parse_refs_enqueue` / `job_papers_needing_assets`
//!   → `Result<ApiResult<…>, String>`;
//! * `job_cancel` → `Result<ApiResult<bool>, String>`.
//!
//! Two execution models, split exactly like the desktop Host:
//!
//! * **renderer offers** — `job_import_enqueue` / `job_layout_analyze_enqueue` /
//!   `job_citing_scan_enqueue` / `job_library_io_enqueue` /
//!   `job_metadata_refresh_enqueue` register the job as `running` and emit
//!   `job:offer` to the requesting tab (`{jobId,kind,vaultPath,paperPath,force,params}`);
//!   the browser executes it and reports back through `job_report`.
//! * **host runners** — `downloadAssets` / `parseBody` / `parseRefs` run the
//!   shared `agentero-core` pipelines inside the TaskCenter. Byte/count progress
//!   still arrives as `job:progress` (core forwards payloads through
//!   `WebHostHooks`), and the `DownloadAssets` runner owns the same follow-ups as
//!   the desktop runner: `PAPER.md` backfill (`ParseBody`) once assets land,
//!   plus the `paper:assets-ready` fact event.
//!
//! `job_import_enqueue` defaults to the renderer offer (desktop semantics); the
//! previous server-side lookup import is kept behind
//! `AGENTERO_WEB_IMPORT_SERVER_FALLBACK=1` for headless/CI runs with no tab.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use agentero_core::features::paper::capabilities::CapsCache;
use agentero_core::features::paper::import::{
    download_paper_assets_with_progress, import_by_identifier_with_progress, LookupImportArgs,
    PaperDownloadAssetsArgs,
};
use agentero_core::features::pdf_parse::{parse_paper_body, PaperParseBodyArgs};

use crate::error::{blocking, fail_msg, parse_args, typed_err, typed_ok, HandlerResult};
use crate::rpc::RpcCtx;
use crate::tasks::{JobKind, JobLane, JobSnapshot, JobSpec, JobState, TaskCenter};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobListArgs {
    #[serde(default)]
    vault_path: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobCancelArgs {
    job_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobReportArgs {
    job_id: String,
    #[serde(default)]
    progress: Option<f32>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    state: Option<JobState>,
}

/// `JobImportEnqueueArgs` — import has no paper folder yet, `path` is the
/// vault-relative destination parent.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobImportEnqueueArgs {
    vault_path: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    lane: Option<JobLane>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    params: Option<Value>,
}

/// `JobEnqueueArgs` — paper-scoped host task (`downloadAssets`, `parseRefs`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobEnqueueArgs {
    vault_path: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    lane: Option<JobLane>,
    #[serde(default)]
    force: bool,
}

/// Which generated parse artifacts a bulk clear/reparse command touches.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum ParseResultScope {
    Layout,
    Paper,
    All,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClearParseResultsArgs {
    vault_path: String,
    scope: ParseResultScope,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClearParseResultsResult {
    papers_scanned: u32,
    files_removed: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClearAndReparseResult {
    papers_scanned: u32,
    files_removed: u32,
    layout_enqueued: u32,
    paper_enqueued: u32,
}

const LAYOUT_SIDECAR_FILES: &[&str] = &["layout.json", "layout-index.json"];
const PAPER_MD_FILE: &str = "PAPER.md";

/// `JobParseBodyEnqueueArgs` — parseBody carries the renderer's cooperative
/// cancel task id; the server runs the job under its own id instead.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobParseBodyEnqueueArgs {
    vault_path: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    lane: Option<JobLane>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    #[allow(dead_code)]
    task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobPapersNeedingAssetsArgs {
    vault_path: String,
}

/// `JobVaultScopeEnqueueArgs` — vault-scope renderer kinds (`CitingScan` /
/// `LibraryIo` / `MetadataRefresh`): no paper target, `params` carries the payload.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobVaultScopeEnqueueArgs {
    vault_path: String,
    #[serde(default)]
    lane: Option<JobLane>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    params: Option<Value>,
}

/// `job:offer` payload (desktop `JobOfferPayload`): the browser executor's task
/// context. `vaultPath` is the canonical vault root, like the desktop offer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JobOfferPayload {
    job_id: String,
    kind: JobKind,
    vault_path: String,
    paper_path: Option<String>,
    force: bool,
    params: Option<Value>,
}

pub async fn dispatch(ctx: &RpcCtx, command: &str, args: &Value) -> Option<HandlerResult> {
    Some(match command {
        "job_list" => job_list(ctx, args).await,
        "job_cancel" => job_cancel(ctx, args).await,
        "job_report" => job_report(ctx, args).await,
        "job_import_enqueue" => job_import_enqueue(ctx, args).await,
        "job_layout_analyze_enqueue" => job_layout_analyze_enqueue(ctx, args).await,
        "job_citing_scan_enqueue" => job_citing_scan_enqueue(ctx, args).await,
        "job_library_io_enqueue" => job_library_io_enqueue(ctx, args).await,
        "job_metadata_refresh_enqueue" => job_metadata_refresh_enqueue(ctx, args).await,
        "job_connector_sync_enqueue" => fail_msg(
            "Zotero Connector is not available in the headless web host",
        ),
        "job_download_assets_enqueue" => job_download_assets_enqueue(ctx, args).await,
        "job_parse_body_enqueue" => job_parse_body_enqueue(ctx, args).await,
        "job_parse_refs_enqueue" => job_parse_refs_enqueue(ctx, args).await,
        "job_papers_needing_assets" => job_papers_needing_assets(ctx, args).await,
        "clear_parse_results" => clear_parse_results(ctx, args).await,
        "clear_and_reparse" => clear_and_reparse(ctx, args).await,
        _ => return None,
    })
}

async fn job_list(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobListArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let jobs = ctx
        .state
        .tasks
        .list(args.vault_path.as_deref(), args.path.as_deref());
    log::debug!("job_list -> {} job(s)", jobs.len());
    typed_ok(jobs)
}

async fn job_cancel(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobCancelArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let cancelled = ctx.state.tasks.cancel(&args.job_id);
    log::debug!("job_cancel {} -> {cancelled}", args.job_id);
    typed_ok(cancelled)
}

async fn job_report(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobReportArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    match ctx.state.tasks.report(
        &args.job_id,
        args.progress,
        args.phase,
        args.error,
        args.state,
    ) {
        Some(snapshot) => typed_ok(snapshot),
        None => fail_msg("job not found"),
    }
}

/// `job_import_enqueue` — renderer-executed import orchestration (magic wand /
/// local PDF / BibTeX), desktop `job_import_enqueue` semantics.
///
/// The job is offered to the requesting browser tab; the frontend import
/// executor drives the multi-command flow and reports progress / terminal state
/// through `job_report`. With `AGENTERO_WEB_IMPORT_SERVER_FALLBACK=1` the
/// identifier (`lookup`) mode runs the server-side core pipeline instead
/// (headless / CI runs with no browser tab).
async fn job_import_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobImportEnqueueArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let mode = args
        .params
        .as_ref()
        .and_then(|params| params.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or("lookup");
    if !server_import_fallback_enabled() || mode != "lookup" {
        let parent_dir = args
            .path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .unwrap_or("papers")
            .replace('\\', "/");
        let parent_dir = match agentero_core::fs::sanitize_vault_rel(&parent_dir) {
            Ok(path) => path,
            Err(error) => return fail_msg(error),
        };
        let spec = match JobSpec::new(JobKind::Import, &args.vault_path) {
            Ok(spec) => spec
                .paper_path(parent_dir)
                .lane(args.lane)
                .force(args.force)
                .params(args.params),
            Err(error) => return fail_msg(error),
        };
        return enqueue_renderer_job(ctx, spec);
    }
    job_import_server_fallback(ctx, args).await
}

/// Server-side identifier import (`AGENTERO_WEB_IMPORT_SERVER_FALLBACK=1`): core
/// resolves the identifier, commits the paper and emits `paper:imported`; no
/// browser tab is involved.
async fn job_import_server_fallback(ctx: &RpcCtx, args: JobImportEnqueueArgs) -> HandlerResult {
    let Some(params) = args.params.as_ref() else {
        return fail_msg("import job requires params");
    };
    let text = params
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let Some(text) = text else {
        return fail_msg("import job is missing its identifier");
    };
    let parent_dir = args
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or("papers")
        .replace('\\', "/");
    let parent_dir = match agentero_core::fs::sanitize_vault_rel(&parent_dir) {
        Ok(path) => path,
        Err(error) => return fail_msg(error),
    };

    let spec = match JobSpec::new(JobKind::Import, &args.vault_path) {
        Ok(spec) => spec
            .paper_path(parent_dir.clone())
            .lane(args.lane)
            .force(args.force)
            .params(Some(params.clone()))
            // Executed by the server in fallback mode, not the browser.
            .host(crate::tasks::ExecHost::Host),
        Err(error) => return fail_msg(error),
    };

    let vault_path = args.vault_path.clone();
    let tasks = ctx.state.tasks.clone();
    let app = ctx.state.host_handle();
    let caps = ctx.state.caps.clone();
    let note_mode = super::import::note_mode(ctx);
    let translator_base = super::import::translator_base(ctx, None);
    let snapshot = tasks.spawn(spec, move |handle| {
        let job_id = handle.id().to_string();
        Box::pin(async move {
            handle.ensure_running()?;
            let lookup = LookupImportArgs {
                vault_path,
                parent_dir,
                text,
                translator_base_url: Some(translator_base),
                task_id: Some(job_id),
            };
            let result =
                import_by_identifier_with_progress(lookup, Some(&app), Some(&caps), note_mode)
                    .await;
            match result {
                Ok(outcome) => {
                    log::info!(
                        "import job finished path={} id={}",
                        outcome.path,
                        outcome.id
                    );
                    Ok(())
                }
                Err(error) => Err(error.to_string()),
            }
        })
    });
    typed_ok(snapshot)
}

/// `job_layout_analyze_enqueue` — renderer offer (browser ONNX/wasm layout pass).
async fn job_layout_analyze_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobEnqueueArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let paper_path = match sanitize_paper_path(args.path.as_deref(), "layoutAnalyze") {
        Ok(path) => path,
        Err(error) => return fail_msg(error),
    };
    let spec = match JobSpec::new(JobKind::LayoutAnalyze, &args.vault_path) {
        Ok(spec) => spec
            .paper_path(paper_path)
            .lane(args.lane)
            .force(args.force),
        Err(error) => return fail_msg(error),
    };
    enqueue_renderer_job(ctx, spec)
}

/// `job_citing_scan_enqueue` — renderer offer (reverse-citation scan UI flow).
async fn job_citing_scan_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    job_vault_scope_renderer_enqueue(ctx, args, JobKind::CitingScan).await
}

/// `job_library_io_enqueue` — renderer offer (dialog-driven bibliography I/O).
async fn job_library_io_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    job_vault_scope_renderer_enqueue(ctx, args, JobKind::LibraryIo).await
}

/// `job_metadata_refresh_enqueue` — renderer offer (bulk metadata batch).
async fn job_metadata_refresh_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    job_vault_scope_renderer_enqueue(ctx, args, JobKind::MetadataRefresh).await
}

async fn job_vault_scope_renderer_enqueue(
    ctx: &RpcCtx,
    args: &Value,
    kind: JobKind,
) -> HandlerResult {
    let args: JobVaultScopeEnqueueArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let spec = match JobSpec::new(kind, &args.vault_path) {
        Ok(spec) => spec
            .lane(args.lane)
            .force(args.force)
            .params(args.params),
        Err(error) => return fail_msg(error),
    };
    enqueue_renderer_job(ctx, spec)
}

/// `true` when `AGENTERO_WEB_IMPORT_SERVER_FALLBACK` opts into the server-side
/// lookup import (headless/CI deployments with no browser tab to execute offers).
fn server_import_fallback_enabled() -> bool {
    std::env::var("AGENTERO_WEB_IMPORT_SERVER_FALLBACK")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Register a renderer-executed job and offer it to the requesting tab.
fn enqueue_renderer_job(ctx: &RpcCtx, spec: JobSpec) -> HandlerResult {
    let job = ctx.state.tasks.spawn_renderer(spec);
    if job.created {
        emit_offer(ctx, &job.snapshot);
    }
    typed_ok(job.snapshot)
}

fn job_offer_payload(snapshot: &JobSnapshot) -> JobOfferPayload {
    JobOfferPayload {
        job_id: snapshot.id.clone(),
        kind: snapshot.kind,
        vault_path: snapshot.vault_path.clone(),
        paper_path: snapshot.paper_path.clone(),
        force: snapshot.force,
        params: snapshot.params.clone(),
    }
}

fn emit_offer(ctx: &RpcCtx, snapshot: &JobSnapshot) {
    let offer = job_offer_payload(snapshot);
    match ctx.window.as_deref() {
        // Targeted offer: only the tab that issued the RPC executes it.
        Some(label) => {
            ctx.state
                .events
                .emit_to(label, crate::tasks::JOB_OFFER_EVENT, &offer)
        }
        // No tab context (startup reconcile / CLI-ish caller): broadcast, the
        // first browser executor claims it.
        None => ctx
            .state
            .events
            .emit(crate::tasks::JOB_OFFER_EVENT, &offer),
    }
    log::debug!(
        "job:offer {} kind={:?} target={:?}",
        snapshot.id,
        snapshot.kind,
        ctx.window
    );
}

/// `job_download_assets_enqueue` — server-side PDF/TeX asset download for one
/// paper folder (desktop executed this in the renderer).
///
/// Follow-ups mirror the desktop runner: invalidate the caps cache, emit
/// `paper:assets-ready`, and enqueue a `ParseBody` job when the paper still has
/// no parsed body.
async fn job_download_assets_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobEnqueueArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let paper_path = match sanitize_paper_path(args.path.as_deref(), "downloadAssets") {
        Ok(path) => path,
        Err(error) => return fail_msg(error),
    };

    let spec = match JobSpec::new(JobKind::DownloadAssets, &args.vault_path) {
        Ok(spec) => spec
            .paper_path(paper_path.clone())
            .lane(args.lane)
            .force(args.force),
        Err(error) => return fail_msg(error),
    };
    let vault_canonical = spec.vault_path.clone();

    let vault_path = args.vault_path.clone();
    let tasks = ctx.state.tasks.clone();
    let follow_up_tasks = tasks.clone();
    let app = ctx.state.host_handle();
    let caps = ctx.state.caps.clone();
    let snapshot = tasks.spawn(spec, move |handle| {
        let job_id = handle.id().to_string();
        Box::pin(async move {
            handle.ensure_running()?;
            let download = PaperDownloadAssetsArgs {
                vault_path: vault_path.clone(),
                path: paper_path.clone(),
                task_id: Some(job_id),
            };
            download_paper_assets_with_progress(download, Some(&app), Some(&caps))
                .await
                .map_err(|error| error.to_string())?;

            let vault_dir = vault_dir(&vault_canonical, &vault_path);
            caps.invalidate(&vault_dir, &paper_path);
            let paper_id = paper_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or_default()
                .to_string();
            agentero_core::features::lifecycle::emit_paper_assets_ready(
                Some(&app),
                &vault_dir,
                &paper_id,
            );

            if !handle.is_cancelled() && caps.caps_for(&vault_dir, &paper_path).needs_paper_md() {
                match enqueue_parse_body(
                    &follow_up_tasks,
                    &caps,
                    &vault_path,
                    &paper_path,
                    false,
                ) {
                    Ok(parse_job) => {
                        log::info!(
                            "downloadAssets follow-up ParseBody job={} paper={paper_path}",
                            parse_job.id
                        );
                    }
                    Err(error) => log::warn!("downloadAssets follow-up parse failed: {error}"),
                }
            }
            Ok(())
        })
    });
    typed_ok(snapshot)
}

/// `job_parse_body_enqueue` — generate `PAPER.md` from the paper's PDF/TeX.
async fn job_parse_body_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobParseBodyEnqueueArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let paper_path = match sanitize_paper_path(args.path.as_deref(), "parseBody") {
        Ok(path) => path,
        Err(error) => return fail_msg(error),
    };
    let tasks = ctx.state.tasks.clone();
    let caps = ctx.state.caps.clone();
    match enqueue_parse_body(&tasks, &caps, &args.vault_path, &paper_path, args.force) {
        Ok(snapshot) => typed_ok(snapshot),
        Err(error) => fail_msg(error),
    }
}

/// `job_parse_refs_enqueue` — parse the paper's reference sidecar.
async fn job_parse_refs_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobEnqueueArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let paper_path = match sanitize_paper_path(args.path.as_deref(), "parseRefs") {
        Ok(path) => path,
        Err(error) => return fail_msg(error),
    };
    match enqueue_parse_refs(&ctx.state.tasks, &args.vault_path, &paper_path, args.force) {
        Ok(snapshot) => typed_ok(snapshot),
        Err(error) => fail_msg(error),
    }
}

/// `job_papers_needing_assets({vaultPath})` → `Result<ApiResult<Vec<String>>, String>`.
///
/// Vault-relative paths still missing local assets (desktop §8.4, CapsCache):
/// no PDF, or body unknown and no TeX / `PAPER.md`. The browser uses this after
/// an import to enqueue `downloadAssets` for the new papers.
async fn job_papers_needing_assets(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobPapersNeedingAssetsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let vault = match crate::error::vault_root(&args.vault_path) {
        Ok(vault) => vault,
        Err(error) => return fail_msg(error),
    };
    let caps = ctx.state.caps.clone();
    let result = blocking(move || {
        let papers = agentero_core::features::catalog::papers::list_all(&vault)
            .map_err(|error| error.to_string())?;
        Ok(papers
            .into_iter()
            .filter(|paper| {
                caps.caps_for(&vault, &paper.path)
                    .needs_asset_download(paper.body_source.as_deref())
            })
            .map(|paper| paper.path)
            .collect::<Vec<_>>())
    })
    .await;
    match result {
        Ok(paths) => typed_ok(paths),
        Err(error) => fail_msg(error),
    }
}

/// `clear_parse_results({vaultPath, scope})` → typed ApiResult.
async fn clear_parse_results(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ClearParseResultsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let cancel_vault = args.vault_path.clone();
    let scope = args.scope;
    cancel_parse_jobs(ctx, &cancel_vault, scope);
    let vault_path = args.vault_path;
    match blocking(move || clear_parse_artifacts(&vault_path, scope)).await {
        Ok((papers_scanned, files_removed, _)) => {
            ctx.state.caps.clear();
            typed_ok(ClearParseResultsResult {
                papers_scanned,
                files_removed,
            })
        }
        Err(error) => fail_msg(error),
    }
}

/// `clear_and_reparse({vaultPath, scope})` — clear artifacts, then enqueue
/// layout offers (browser executor) and server-side `PAPER.md` parses.
async fn clear_and_reparse(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ClearParseResultsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let vault_path = args.vault_path.clone();
    let scope = args.scope;
    cancel_parse_jobs(ctx, &vault_path, scope);
    let clear_path = vault_path.clone();
    let (papers_scanned, files_removed, affected) =
        match blocking(move || clear_parse_artifacts(&clear_path, scope)).await {
            Ok(result) => result,
            Err(error) => return fail_msg(error),
        };

    ctx.state.caps.clear();
    let mut layout_enqueued = 0u32;
    let mut paper_enqueued = 0u32;
    for path in affected {
        if matches!(scope, ParseResultScope::Layout | ParseResultScope::All) {
            if let Ok(spec) = JobSpec::new(JobKind::LayoutAnalyze, &vault_path) {
                let spec = spec
                    .paper_path(path.clone())
                    .lane(Some(JobLane::Idle))
                    .force(true);
                if enqueue_renderer_job(ctx, spec).is_ok() {
                    layout_enqueued += 1;
                }
            }
        }
        if matches!(scope, ParseResultScope::Paper | ParseResultScope::All) {
            if enqueue_parse_body(&ctx.state.tasks, &ctx.state.caps, &vault_path, &path, true)
                .is_ok()
            {
                paper_enqueued += 1;
            }
        }
    }
    typed_ok(ClearAndReparseResult {
        papers_scanned,
        files_removed,
        layout_enqueued,
        paper_enqueued,
    })
}

fn cancel_parse_jobs(ctx: &RpcCtx, vault_path: &str, scope: ParseResultScope) {
    let kinds: &[JobKind] = match scope {
        ParseResultScope::Layout => &[JobKind::LayoutAnalyze],
        ParseResultScope::Paper => &[JobKind::ParseBody],
        ParseResultScope::All => &[JobKind::LayoutAnalyze, JobKind::ParseBody],
    };
    for job in ctx.state.tasks.list(Some(vault_path), None) {
        if matches!(job.state, JobState::Queued | JobState::Running)
            && kinds.contains(&job.kind)
        {
            ctx.state.tasks.cancel(&job.id);
        }
    }
}

fn clear_parse_artifacts(
    vault_path: &str,
    scope: ParseResultScope,
) -> Result<(u32, u32, Vec<String>), String> {
    let vault = crate::error::vault_root(vault_path)?;
    let papers = agentero_core::features::catalog::papers::list_all(&vault)
        .map_err(|error| error.to_string())?;
    let papers_scanned = papers.len() as u32;
    let mut files_removed = 0u32;
    let mut affected = Vec::with_capacity(papers.len());
    for paper in papers {
        let paper_dir = vault.join(&paper.path);
        if !paper_dir.is_dir() {
            continue;
        }
        affected.push(paper.path);
        if matches!(scope, ParseResultScope::Layout | ParseResultScope::All) {
            let source_dir = paper_dir.join("source");
            for file in LAYOUT_SIDECAR_FILES {
                files_removed += u32::from(remove_file_best_effort(&source_dir.join(file))?);
            }
        }
        if matches!(scope, ParseResultScope::Paper | ParseResultScope::All) {
            files_removed += u32::from(remove_file_best_effort(&paper_dir.join(PAPER_MD_FILE))?);
        }
    }
    // Capability probes cache PDF/body/layout presence; clear them so the next
    // UI read reflects the deleted files.
    Ok((papers_scanned, files_removed, affected))
}

fn remove_file_best_effort(path: &Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("remove {}: {error}", path.display())),
    }
}

/// Enqueue a `ParseBody` job (also used by the download-assets follow-up).
fn enqueue_parse_body(
    tasks: &TaskCenter,
    caps: &CapsCache,
    vault_path: &str,
    paper_path: &str,
    force: bool,
) -> Result<JobSnapshot, String> {
    let spec = JobSpec::new(JobKind::ParseBody, vault_path)?
        .paper_path(paper_path)
        .force(force);
    let caps = caps.clone();
    let vault_owned = vault_path.to_string();
    let path_owned = paper_path.to_string();
    Ok(tasks.spawn(spec, move |handle| {
        let job_id = handle.id().to_string();
        Box::pin(async move {
            handle.ensure_running()?;
            let args = PaperParseBodyArgs {
                vault_path: vault_owned,
                path: path_owned,
                force,
                task_id: Some(job_id),
            };
            match parse_paper_body(args, Some(&caps)).await {
                // A skipped or successful parse returns Ok with no error; a real
                // liteparse failure also returns Ok, carrying the reason.
                Ok(parsed) => match parsed.error {
                    Some(message) => Err(message),
                    None => Ok(()),
                },
                Err(error) => Err(error.to_string()),
            }
        })
    }))
}

/// Enqueue a `ParseRefs` job (reference sidecar; online lookup enabled).
fn enqueue_parse_refs(
    tasks: &TaskCenter,
    vault_path: &str,
    paper_path: &str,
    force: bool,
) -> Result<JobSnapshot, String> {
    let spec = JobSpec::new(JobKind::ParseRefs, vault_path)?
        .paper_path(paper_path)
        .force(force);
    let vault_owned = vault_path.to_string();
    let path_owned = paper_path.to_string();
    Ok(tasks.spawn(spec, move |_handle| {
        Box::pin(async move {
            let vault =
                agentero_core::fs::resolve_vault(&vault_owned).map_err(|error| error.to_string())?;
            agentero_core::features::refs::parse_paper_refs(&vault, &path_owned, true, force)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }))
}

/// Canonical vault dir when it still exists, otherwise the caller string.
fn vault_dir(canonical: &str, fallback: &str) -> PathBuf {
    let path = PathBuf::from(canonical);
    if path.is_dir() {
        path
    } else {
        PathBuf::from(fallback)
    }
}

fn sanitize_paper_path(raw: Option<&str>, command: &str) -> Result<String, String> {
    let trimmed = raw
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| format!("{command} requires a paper path"))?;
    agentero_core::fs::sanitize_vault_rel(&trimmed)
}
