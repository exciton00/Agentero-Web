//! Server-side task center handlers.
//!
//! Wire contract (`src/lib/core/bindings.ts`):
//! * `job_list` / `job_report` / `job_import_enqueue` / `job_download_assets_enqueue`
//!   → `Result<ApiResult<…>, String>`;
//! * `job_cancel` → `Result<ApiResult<bool>, String>`.
//!
//! Jobs the desktop executed in the renderer (`import`, `downloadAssets`) run
//! on the server here through the shared `agentero-core` pipelines, so the
//! browser does not need the multi-command orchestration. Progress still
//! arrives as `job:progress` (core forwards byte/count payloads through
//! `WebHostHooks`).

use serde::Deserialize;
use serde_json::Value;

use agentero_core::features::paper::import::{
    download_paper_assets_with_progress, import_by_identifier_with_progress, LookupImportArgs,
    NoteShellMode, PaperDownloadAssetsArgs,
};

use crate::error::{fail_msg, parse_args, typed_err, typed_ok, HandlerResult};
use crate::rpc::RpcCtx;
use crate::tasks::{JobKind, JobLane, JobSpec, JobState};

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

/// `JobEnqueueArgs` — paper-scoped host task (`downloadAssets`).
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

pub async fn dispatch(ctx: &RpcCtx, command: &str, args: &Value) -> Option<HandlerResult> {
    Some(match command {
        "job_list" => job_list(ctx, args).await,
        "job_cancel" => job_cancel(ctx, args).await,
        "job_report" => job_report(ctx, args).await,
        "job_import_enqueue" => job_import_enqueue(ctx, args).await,
        "job_download_assets_enqueue" => job_download_assets_enqueue(ctx, args).await,
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

/// `job_import_enqueue` — server-executed import.
///
/// v1 supports the identifier magic-wand mode (`params.mode == "lookup"`,
/// `params.text`), which maps directly onto
/// `agentero_core::features::paper::import::import_by_identifier_with_progress`
/// (the same pipeline the CLI uses). Renderer-only modes (skill / plaza /
/// papers.cool UI pickers) report a business error instead of hanging.
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
    let snapshot = tasks.spawn(spec, move |handle| {
        let job_id = handle.id().to_string();
        Box::pin(async move {
            handle.ensure_running()?;
            let lookup = LookupImportArgs {
                vault_path,
                parent_dir,
                text,
                translator_base_url: None,
                task_id: Some(job_id),
            };
            let result = import_by_identifier_with_progress(
                lookup,
                Some(&app),
                Some(&caps),
                NoteShellMode::Standard,
            )
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
async fn job_download_assets_enqueue(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JobEnqueueArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let paper_path = match args
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        Some(path) => match agentero_core::fs::sanitize_vault_rel(&path.replace('\\', "/")) {
            Ok(path) => path,
            Err(error) => return fail_msg(error),
        },
        None => return fail_msg("downloadAssets requires a paper path"),
    };

    let spec = match JobSpec::new(JobKind::DownloadAssets, &args.vault_path) {
        Ok(spec) => spec
            .paper_path(paper_path.clone())
            .lane(args.lane)
            .force(args.force),
        Err(error) => return fail_msg(error),
    };

    let vault_path = args.vault_path.clone();
    let tasks = ctx.state.tasks.clone();
    let app = ctx.state.host_handle();
    let caps = ctx.state.caps.clone();
    let snapshot = tasks.spawn(spec, move |handle| {
        let job_id = handle.id().to_string();
        Box::pin(async move {
            handle.ensure_running()?;
            let download = PaperDownloadAssetsArgs {
                vault_path,
                path: paper_path,
                task_id: Some(job_id),
            };
            download_paper_assets_with_progress(download, Some(&app), Some(&caps))
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    });
    typed_ok(snapshot)
}
