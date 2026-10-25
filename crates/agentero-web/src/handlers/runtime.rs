//! Runtime handler registry (watcher / jobs / sync / import).
//!
//! Owned by the runtime layer; `rpc.rs` asks it after the domain registry and
//! before the platform match table. Command names, argument shapes and return
//! envelopes follow `src/lib/core/bindings.ts` exactly.
//!
//! `jobs` / `sync` / `import` live in their own files but are declared here
//! (with an explicit `#[path]`) because `handlers/mod.rs` is owned by the
//! platform layer; a future cleanup can hoist them to plain `pub mod`
//! declarations.

use serde::Deserialize;
use serde_json::Value;

use crate::error::{typed_err, HandlerResult};
use crate::rpc::RpcCtx;

#[path = "import.rs"]
pub mod import;
#[path = "jobs.rs"]
pub mod jobs;
#[path = "sync.rs"]
pub mod sync;

pub async fn dispatch(ctx: &RpcCtx, command: &str, args: &Value) -> Option<HandlerResult> {
    match command {
        // ---- Vault filesystem watcher (per-tab, reference counted) ----
        "fs_watch_start" => Some(fs_watch_start(ctx, args).await),
        "fs_watch_stop" => Some(fs_watch_stop(ctx).await),
        // ---- Task center: registry + host runners + renderer offers ----
        "job_list"
        | "job_cancel"
        | "job_report"
        | "job_import_enqueue"
        | "job_layout_analyze_enqueue"
        | "job_citing_scan_enqueue"
        | "job_library_io_enqueue"
        | "job_metadata_refresh_enqueue"
        | "job_connector_sync_enqueue"
        | "job_download_assets_enqueue"
        | "job_parse_body_enqueue"
        | "job_parse_refs_enqueue"
        | "job_jev_smart_highlights_enqueue"
        | "jev_probe_health"
        | "job_papers_needing_assets"
        | "clear_parse_results"
        | "clear_and_reparse" => jobs::dispatch(ctx, command, args).await,
        // ---- Vault cloud sync (S3 / WebDAV / Git) ----
        "sync_get_status" | "sync_configure" | "sync_disconnect" | "sync_now"
        | "sync_scope_sizes" => sync::dispatch(ctx, command, args).await,
        // ---- Import: local PDFs / identifiers / bibliography / skills ----
        "paper_import_local_pdf"
        | "paper_import"
        | "paper_resolve_identifier"
        | "lookup_import_batch"
        | "paper_stage_import_file"
        | "paper_export"
        | "skill_install"
        | "skill_discard"
        | "paper_coolpapers_notes"
        | "paper_coolpapers_import" => import::dispatch(ctx, command, args).await,
        _ => None,
    }
}


/// Tab label issuing this RPC: the `X-Agentero-Window` header set by the web
/// transport shim, with the same `main` fallback the WS endpoint uses.
fn window_label(ctx: &RpcCtx) -> String {
    ctx.window.clone().unwrap_or_else(|| "main".to_string())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FsWatchStartArgs {
    vault_path: String,
}

/// `fs_watch_start(vaultPath)` → `Result<null, String>` (no ApiResult wrapper).
async fn fs_watch_start(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: FsWatchStartArgs = match serde_json::from_value(args.clone()) {
        Ok(args) => args,
        Err(error) => return typed_err(format!("invalid_args: {error}")),
    };
    let label = window_label(ctx);
    match ctx.state.watcher.start(&label, &args.vault_path) {
        Ok(()) => {
            log::debug!("fs_watch_start label={label} vault={}", args.vault_path);
            Ok(Value::Null)
        }
        Err(error) => typed_err(error),
    }
}

/// `fs_watch_stop()` → `Result<null, String>` (no-op when not watching).
async fn fs_watch_stop(ctx: &RpcCtx) -> HandlerResult {
    let label = window_label(ctx);
    ctx.state.watcher.stop(&label);
    log::debug!("fs_watch_stop label={label}");
    Ok(Value::Null)
}
