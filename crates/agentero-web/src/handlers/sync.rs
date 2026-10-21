//! Vault cloud sync handlers (S3 / WebDAV / Git).
//!
//! Thin shell over `agentero_core::features::sync` — the same tauri-free engine
//! the desktop Host and the `agentero sync` CLI use, so merge/conflict/failure
//! semantics are identical. Credentials live in the server's XDG `sync.json`
//! (keyed by vault root, never inside the vault).
//!
//! Wire contract (`src/lib/core/bindings.ts`, all `Result<ApiResult<T>, String>`):
//! `sync_get_status` / `sync_configure` / `sync_disconnect` / `sync_now` /
//! `sync_scope_sizes`. Long `sync_now` passes stream `sync:state` /
//! `sync:progress` events (payloads mirror `src-tauri/src/app/events_contract.rs`).
//!
//! The auto-sync scheduler is intentionally not started in v1: the server has
//! no "vault open" lifecycle, manual `sync_now` covers the settings-pane flow.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use agentero_core::error::AppError;
use agentero_core::features::sync::config::{self as sync_config, SyncBackendConfig};
use agentero_core::features::sync::snapshot;
use agentero_core::features::sync::{engine, local};

use crate::error::{blocking, fail, fail_msg, parse_args, typed_err, typed_ok, HandlerResult};
use crate::events::EventBus;
use crate::rpc::RpcCtx;

/// Sync event names, unchanged from the desktop Host.
pub const SYNC_STATE_EVENT: &str = "sync:state";
pub const SYNC_PROGRESS_EVENT: &str = "sync:progress";

/// Per-vault "one sync pass at a time" lease for this server process. The core
/// engine additionally takes a cross-process lock (`sync_vault_locked`).
pub struct SyncService {
    running: Mutex<HashSet<String>>,
    events: EventBus,
}

impl SyncService {
    pub fn new(events: EventBus) -> Self {
        Self {
            running: Mutex::new(HashSet::new()),
            events,
        }
    }

    pub fn is_running(&self, vault_key: &str) -> bool {
        self.running
            .lock()
            .map(|running| running.contains(vault_key))
            .unwrap_or(false)
    }

    fn try_begin(&self, vault_key: &str) -> bool {
        self.running
            .lock()
            .map(|mut running| running.insert(vault_key.to_string()))
            .unwrap_or(false)
    }

    fn end(&self, vault_key: &str) {
        if let Ok(mut running) = self.running.lock() {
            running.remove(vault_key);
        }
    }

    fn emit_state(&self, vault_path: &str, status: &str, error: Option<String>) {
        self.events.emit(
            SYNC_STATE_EVENT,
            &SyncStateEvent {
                vault_path,
                status,
                error,
            },
        );
    }

    fn emit_progress(&self, vault_path: &str, phase: &str, current: usize, total: usize) {
        self.events.emit(
            SYNC_PROGRESS_EVENT,
            &SyncProgressEvent {
                vault_path,
                phase,
                current,
                total,
            },
        );
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncStateEvent<'a> {
    vault_path: &'a str,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncProgressEvent<'a> {
    vault_path: &'a str,
    phase: &'a str,
    current: usize,
    total: usize,
}

/// `SyncStatus` wire shape (`SyncStatus_Serialize` in bindings.ts).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncStatus {
    configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<SyncBackendConfig>,
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_sync_at: Option<String>,
    last_version: u64,
}

/// `SyncScopeSizes` wire shape.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncScopeSizes {
    pdf: u64,
    source: u64,
    attachments: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncVaultArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncConfigureArgs {
    vault_path: String,
    config: SyncBackendConfig,
}

pub async fn dispatch(
    ctx: &RpcCtx,
    command: &str,
    args: &serde_json::Value,
) -> Option<HandlerResult> {
    Some(match command {
        "sync_get_status" => get_status(ctx, args).await,
        "sync_configure" => configure(ctx, args).await,
        "sync_disconnect" => disconnect(ctx, args).await,
        "sync_now" => sync_now(ctx, args).await,
        "sync_scope_sizes" => scope_sizes(ctx, args).await,
        _ => return None,
    })
}

/// Local vault directory for sync operations (remote handles are rejected).
fn vault_dir(vault_path: &str) -> Result<PathBuf, AppError> {
    let trimmed = vault_path.trim();
    if trimmed.is_empty() || trimmed.starts_with("remote:") {
        return Err(AppError::message("sync requires a local vault"));
    }
    agentero_core::fs::resolve_vault(trimmed)
}

/// Resolve the `sync.json` entry for a vault dir. The desktop keys entries by
/// the path string its frontend passed, which can differ in normalization from
/// the server's canonical path; match by canonical path like the CLI does so
/// both sides keep sharing one entry.
fn entry_for(dir: &Path) -> (String, Option<SyncBackendConfig>) {
    let key = dir.to_string_lossy().to_string();
    if let Some(cfg) = sync_config::get(&key) {
        return (key, Some(cfg));
    }
    let want = agentero_core::fs::canonicalize_best_effort(dir);
    for (stored, cfg) in sync_config::list_all() {
        if agentero_core::fs::canonicalize_best_effort(Path::new(&stored)) == want {
            return (stored, Some(cfg));
        }
    }
    (key, None)
}

fn status_for(dir: &Path, running: bool) -> SyncStatus {
    let (_, config) = entry_for(dir);
    let meta = local::read_meta(dir);
    SyncStatus {
        configured: config.is_some(),
        config: config.map(|cfg| cfg.masked()),
        running,
        last_sync_at: meta.last_sync_at,
        last_version: meta.last_version,
    }
}

async fn get_status(ctx: &RpcCtx, args: &serde_json::Value) -> HandlerResult {
    let args: SyncVaultArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let inner = || -> Result<SyncStatus, AppError> {
        let dir = vault_dir(&args.vault_path)?;
        let key = args.vault_path.trim().to_string();
        Ok(status_for(&dir, ctx.state.sync.is_running(&key)))
    };
    match inner() {
        Ok(status) => typed_ok(status),
        Err(error) => fail(error),
    }
}

/// Validate + connection-test + persist the binding for a vault (same flow as
/// the desktop `sync_configure`: masks restore stored secrets, then the
/// backend probe decides conditional-write support).
async fn configure(ctx: &RpcCtx, args: &serde_json::Value) -> HandlerResult {
    let args: SyncConfigureArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let inner = async {
        let dir = vault_dir(&args.vault_path)?;
        let (key, existing) = entry_for(&dir);
        let mut cfg = args.config.normalized();
        cfg.merge_mask(existing.as_ref());
        cfg.validate()?;
        cfg.conditional_writes = engine::test_connection(&dir, &cfg).await?;
        sync_config::set(&key, cfg.clone())?;
        let meta = local::read_meta(&dir);
        Ok::<_, AppError>(SyncStatus {
            configured: true,
            config: Some(cfg.masked()),
            running: ctx.state.sync.is_running(args.vault_path.trim()),
            last_sync_at: meta.last_sync_at,
            last_version: meta.last_version,
        })
    };
    match inner.await {
        Ok(status) => typed_ok(status),
        Err(error) => fail(error),
    }
}

async fn disconnect(_ctx: &RpcCtx, args: &serde_json::Value) -> HandlerResult {
    let args: SyncVaultArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let inner = || -> Result<(), AppError> {
        let dir = vault_dir(&args.vault_path)?;
        let (key, _) = entry_for(&dir);
        sync_config::remove(&key)?;
        local::clear(&dir);
        Ok(())
    };
    match inner() {
        Ok(()) => typed_ok(()),
        Err(error) => fail(error),
    }
}

async fn sync_now(ctx: &RpcCtx, args: &serde_json::Value) -> HandlerResult {
    let args: SyncVaultArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let vault_key = args.vault_path.trim().to_string();
    let service = ctx.state.sync.clone();

    let prep = (|| -> Result<(PathBuf, SyncBackendConfig), AppError> {
        let dir = vault_dir(&args.vault_path)?;
        let (_, config) = entry_for(&dir);
        let config = config.ok_or_else(|| AppError::message("sync is not configured"))?;
        Ok((dir, config))
    })();
    let (dir, cfg) = match prep {
        Ok(prepared) => prepared,
        Err(error) => return fail(error),
    };
    if !service.try_begin(&vault_key) {
        return fail(AppError::message("sync already running"));
    }

    service.emit_state(&vault_key, "syncing", None);
    let progress_service = service.clone();
    let progress_key = vault_key.clone();
    let progress = move |phase: &str, current: usize, total: usize| {
        progress_service.emit_progress(&progress_key, phase, current, total);
    };
    let result = engine::sync_vault_locked(&dir, &cfg, &progress).await;
    // Release the lease before the terminal event: status consumers may re-read
    // immediately, and the desktop Host does the same.
    service.end(&vault_key);
    match result {
        Ok(outcome) => {
            log::info!(
                "sync finished vault={vault_key} version={} up={} down={} conflicts={}",
                outcome.version,
                outcome.uploaded,
                outcome.downloaded,
                outcome.conflict_copies.len()
            );
            service.emit_state(&vault_key, "idle", None);
            typed_ok(outcome)
        }
        Err(error) => {
            let message = error.to_string();
            log::warn!("sync failed vault={vault_key}: {message}");
            service.emit_state(&vault_key, "error", Some(message));
            fail(error)
        }
    }
}

async fn scope_sizes(_ctx: &RpcCtx, args: &serde_json::Value) -> HandlerResult {
    let args: SyncVaultArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let dir = match vault_dir(&args.vault_path) {
        Ok(dir) => dir,
        Err(error) => return fail(error),
    };
    let result = blocking(move || {
        let mut sizes = SyncScopeSizes::default();
        let walker = walkdir::WalkDir::new(&dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0
                    || !snapshot::is_ignored_name(&entry.file_name().to_string_lossy())
            });
        for entry in walker.flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let Some(rel) = entry
                .path()
                .strip_prefix(&dir)
                .ok()
                .and_then(|path| path.to_str())
                .map(|path| path.replace('\\', "/"))
            else {
                continue;
            };
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            match snapshot::scope_category(&rel) {
                Some("pdf") => sizes.pdf += size,
                Some("source") => sizes.source += size,
                Some("attachments") => sizes.attachments += size,
                _ => {}
            }
        }
        Ok(sizes)
    })
    .await;
    match result {
        Ok(sizes) => typed_ok(sizes),
        Err(error) => fail_msg(error),
    }
}
