//! Server-side task center handlers and host-run job enqueues.
//!
//! Wire contract (`src/lib/core/bindings.ts`):
//! * `job_list` / `job_report` / `job_import_enqueue` / `job_download_assets_enqueue` /
//!   `job_parse_body_enqueue` / `job_parse_refs_enqueue` / `job_papers_needing_assets`
//!   → `Result<ApiResult<…>, String>`;
//! * `job_cancel` → `Result<ApiResult<bool>, String>`.
//!
//! Jobs the desktop executed in the renderer (`import`, `downloadAssets`) or in
//! Host runners (`parseBody`, `parseRefs`) run on the server here through the
//! shared `agentero-core` pipelines, so the browser does not need the
//! multi-command orchestration. Byte/count progress still arrives as
//! `job:progress` (core forwards payloads through `WebHostHooks`), and the
//! `DownloadAssets` runner owns the same follow-ups as the desktop runner:
//! `PAPER.md` backfill (`ParseBody`) once assets land, plus the
//! `paper:assets-ready` fact event.

use std::path::PathBuf;

use serde::Deserialize;
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

pub async fn dispatch(ctx: &RpcCtx, command: &str, args: &Value) -> Option<HandlerResult> {
    Some(match command {
        "job_list" => job_list(ctx, args).await,
        "job_cancel" => job_cancel(ctx, args).await,
        "job_report" => job_report(ctx, args).await,
        "job_import_enqueue" => job_import_enqueue(ctx, args).await,
        "job_download_assets_enqueue" => job_download_assets_enqueue(ctx, args).await,
        "job_parse_body_enqueue" => job_parse_body_enqueue(ctx, args).await,
        "job_parse_refs_enqueue" => job_parse_refs_enqueue(ctx, args).await,
        "job_papers_needing_assets" => job_papers_needing_assets(ctx, args).await,
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
        None => fail_msg("job not found or not running"),
    }
}

/// `job_import_enqueue` — server-executed identifier import.
///
/// Supports the identifier magic-wand mode (`params.mode == "lookup"`,
/// `params.text`), which maps directly onto
/// `agentero_core::features::paper::import::import_by_identifier_with_progress`
/// (the same pipeline the CLI uses). Renderer-only modes (skill / plaza /
/// papers.cool UI pickers) report a business error instead of hanging;
/// `lookup_import_batch` covers the batch/search flows.
async fn job_import_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobImportEnqueueArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let Some(params) = args.params.as_ref() else {
        return fail_msg("import job requires params");
    };
    let mode = params
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("lookup");
    if mode != "lookup" {
        return fail_msg(format!(
            "import mode \"{mode}\" is not supported by the headless web host"
        ));
    }
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
            .params(Some(params.clone())),
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
