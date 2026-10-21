//! Vault cloud sync handlers (S3 / WebDAV / Git) + the Web auto-sync scheduler.
//!
//! Thin shell over `agentero_core::features::sync` — the same tauri-free engine
//! the desktop Host and the `agentero sync` CLI use, so merge/conflict/failure
//! semantics are identical. Credentials live in the server's XDG `sync.json`
//! (keyed by vault root, never inside the vault).
//!
//! Wire contract (`src/lib/core/bindings.ts`, all `Result<ApiResult<T>, String>`):
//! `sync_get_status` / `sync_configure` / `sync_disconnect` / `sync_now` /
//! `sync_scope_sizes`. Long passes stream `sync:state` / `sync:progress`
//! events (payloads mirror `src-tauri/src/app/events_contract.rs`).
//!
//! Unlike the desktop (which schedules auto sync from its vault-open lifecycle)
//! the headless server runs one process-wide scheduler over every configured
//! vault with `autoSync=true`: a 5 s tick re-reads `sync.json`, so
//! configure/disconnect take effect immediately, and each due vault runs the
//! same leased pass as manual `sync_now` (one pass at a time per vault).
//!
//! `sync_get_status` also surfaces the persisted last result:
//! `lastOutcome` (from `.agentero/sync/state.json`) and `lastError`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use agentero_core::error::AppError;
use agentero_core::features::sync::config::{self as sync_config, SyncBackendConfig};
use agentero_core::features::sync::engine::{self, SyncOutcome};
use agentero_core::features::sync::local::{self, LastSyncOutcome};
use agentero_core::features::sync::snapshot;

use crate::error::{blocking, fail, fail_msg, parse_args, typed_err, typed_ok, HandlerResult};
use crate::events::EventBus;
use crate::rpc::RpcCtx;

/// Sync event names, unchanged from the desktop Host.
pub const SYNC_STATE_EVENT: &str = "sync:state";
pub const SYNC_PROGRESS_EVENT: &str = "sync:progress";

/// Scheduler re-read interval: bounds how quickly configure/disconnect and new
/// due times take effect.
const AUTO_SYNC_TICK: Duration = Duration::from_secs(5);
/// The scheduler never runs more often than once a minute, matching the
/// settings contract ("intervalMinutes, minimum 1").
const AUTO_SYNC_MIN_INTERVAL_MINUTES: u32 = 1;

/// Per-vault "one sync pass at a time" lease for this server process. The core
/// engine additionally takes a cross-process lock (`sync_vault_locked`).
pub struct SyncService {
    running: Mutex<HashSet<String>>,
    /// Wall-clock time of each vault's next scheduled automatic pass and the
    /// interval it was scheduled with (so a settings change reschedules).
    next_run: Mutex<HashMap<String, (SystemTime, u32)>>,
    events: EventBus,
}

impl SyncService {
    pub fn new(events: EventBus) -> Self {
        Self {
            running: Mutex::new(HashSet::new()),
            next_run: Mutex::new(HashMap::new()),
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

    /// ISO 8601 timestamp of the next automatic pass for `vault_key`, when the
    /// scheduler has this vault queued.
    pub fn auto_sync_next_run_at(&self, vault_key: &str) -> Option<String> {
        let next_run = self.next_run.lock().ok()?;
        next_run
            .get(vault_key.trim())
            .map(|(at, _)| local::rfc3339_millis_from_system_time(*at))
    }

    /// One sync pass with the per-vault lease and progress events. Shared by
    /// manual `sync_now` and the auto-sync scheduler so both stay mutually
    /// exclusive (the lease is process-wide; the core lock is cross-process).
    pub async fn run_pass(
        self: &Arc<Self>,
        dir: &Path,
        cfg: &SyncBackendConfig,
        vault_key: &str,
    ) -> Result<SyncOutcome, AppError> {
        if !self.try_begin(vault_key) {
            return Err(AppError::message("sync already running"));
        }
        self.emit_state(vault_key, "syncing", None);
        let progress_service = Arc::clone(self);
        let progress_key = vault_key.to_string();
        let progress = move |phase: &str, current: usize, total: usize| {
            progress_service.emit_progress(&progress_key, phase, current, total);
        };
        let result = engine::sync_vault_locked(dir, cfg, &progress).await;
        // Release the lease before the terminal event: status consumers may
        // re-read immediately, and the desktop Host does the same.
        self.end(vault_key);
        match &result {
            Ok(_) => self.emit_state(vault_key, "idle", None),
            Err(error) => self.emit_state(vault_key, "error", Some(error.to_string())),
        }
        result
    }

    /// Start the process-wide auto-sync loop (idempotent per server process).
    /// The task lives until the tokio runtime shuts down.
    pub fn start_auto_sync(self: Arc<Self>) {
        tokio::spawn(async move {
            log::info!(target: "agentero::sync", "auto-sync scheduler started");
            loop {
                tokio::time::sleep(AUTO_SYNC_TICK).await;
                self.auto_sync_tick().await;
            }
        });
    }

    /// Re-read `sync.json`, compute due vaults, and start their passes.
    async fn auto_sync_tick(self: &Arc<Self>) {
        let configs = sync_config::list_all();
        let now = SystemTime::now();
        let mut due: Vec<(String, SyncBackendConfig)> = Vec::new();
        {
            let mut next_run = self
                .next_run
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut configured_auto: HashSet<String> = HashSet::new();
            for (vault_key, cfg) in configs {
                if !cfg.auto_sync {
                    next_run.remove(&vault_key);
                    continue;
                }
                configured_auto.insert(vault_key.clone());
                let interval_minutes = cfg
                    .interval_minutes
                    .max(AUTO_SYNC_MIN_INTERVAL_MINUTES);
                let is_due = match next_run.get(&vault_key) {
                    // A settings change (interval edited) reschedules now:
                    // configure is advertised to take effect immediately.
                    Some((next, scheduled_minutes)) => {
                        now >= *next || *scheduled_minutes != interval_minutes
                    }
                    None => true,
                };
                if is_due {
                    next_run.insert(
                        vault_key.clone(),
                        (now + auto_sync_interval(&cfg), interval_minutes),
                    );
                    due.push((vault_key, cfg));
                }
            }
            // Drop entries for vaults that were disconnected or turned auto off.
            next_run.retain(|vault_key, _| configured_auto.contains(vault_key));
        }
        for (vault_key, cfg) in due {
            if self.is_running(&vault_key) {
                log::info!(
                    target: "agentero::sync",
                    "auto sync skipped vault={vault_key}: a sync pass is already running"
                );
                continue;
            }
            let dir = match vault_dir(&vault_key) {
                Ok(dir) => dir,
                Err(error) => {
                    log::warn!(
                        target: "agentero::sync",
                        "auto sync skipped vault={vault_key}: {error}"
                    );
                    continue;
                }
            };
            let service = Arc::clone(self);
            tokio::spawn(async move {
                log::info!(
                    target: "agentero::sync",
                    "auto sync start vault={vault_key} interval={}m",
                    cfg.interval_minutes
                );
                match service.run_pass(&dir, &cfg, &vault_key).await {
                    Ok(outcome) => log::info!(
                        target: "agentero::sync",
                        "auto sync ok vault={vault_key} version={} up={} down={}",
                        outcome.version,
                        outcome.uploaded,
                        outcome.downloaded
                    ),
                    Err(error) => log::warn!(
                        target: "agentero::sync",
                        "auto sync failed vault={vault_key}: {error}"
                    ),
                }
            });
        }
    }
}

/// Automatic pass cadence for one vault (settings interval floored at 1 minute).
fn auto_sync_interval(cfg: &SyncBackendConfig) -> Duration {
    let minutes = cfg
        .interval_minutes
        .max(AUTO_SYNC_MIN_INTERVAL_MINUTES) as u64;
    Duration::from_secs(minutes * 60)
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

/// `SyncStatus_Serialize` wire shape plus the persisted result fields and the
/// scheduler's next-run estimate.
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
    /// Summary of the last successful pass (`null` before the first success).
    last_outcome: Option<LastSyncOutcome>,
    /// Last failed pass's error (`null` after a success).
    last_error: Option<String>,
    /// Next scheduled automatic pass; `null` when auto sync is off/unconfigured.
    auto_sync_next_run_at: Option<String>,
}

/// `SyncScopeSizes` wire shape: bytes per scope plus the file counts the panel
/// shows as "N files (X MB)".
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncScopeSizes {
    pdf: u64,
    source: u64,
    attachments: u64,
    pdf_files: u64,
    source_files: u64,
    attachment_files: u64,
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

fn status_for(service: &SyncService, dir: &Path, fallback_key: &str) -> SyncStatus {
    let (stored_key, config) = entry_for(dir);
    let meta = local::read_meta(dir);
    // Status must agree with the scheduler/lease key: the stored `sync.json`
    // key when configured, otherwise whatever the caller passed.
    let lease_key = if config.is_some() {
        stored_key.as_str()
    } else {
        fallback_key
    };
    SyncStatus {
        configured: config.is_some(),
        config: config.map(|cfg| cfg.masked()),
        running: service.is_running(lease_key),
        last_sync_at: meta.last_sync_at,
        last_version: meta.last_version,
        last_outcome: meta.last_outcome,
        last_error: meta.last_error,
        auto_sync_next_run_at: service.auto_sync_next_run_at(lease_key),
    }
}

async fn get_status(ctx: &RpcCtx, args: &serde_json::Value) -> HandlerResult {
    let args: SyncVaultArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let inner = || -> Result<SyncStatus, AppError> {
        let dir = vault_dir(&args.vault_path)?;
        Ok(status_for(
            ctx.state.sync.as_ref(),
            &dir,
            args.vault_path.trim(),
        ))
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
        // The scheduler picks the new config up on its next tick (<=5 s); no
        // restart or extra wakeup is needed.
        let meta = local::read_meta(&dir);
        Ok::<_, AppError>(SyncStatus {
            configured: true,
            config: Some(cfg.masked()),
            running: ctx.state.sync.is_running(&key),
            last_sync_at: meta.last_sync_at,
            last_version: meta.last_version,
            last_outcome: meta.last_outcome,
            last_error: meta.last_error,
            auto_sync_next_run_at: ctx.state.sync.auto_sync_next_run_at(&key),
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
    match service.run_pass(&dir, &cfg, &vault_key).await {
        Ok(outcome) => {
            log::info!(
                "sync finished vault={vault_key} version={} up={} down={} conflicts={}",
                outcome.version,
                outcome.uploaded,
                outcome.downloaded,
                outcome.conflict_copies.len()
            );
            typed_ok(outcome)
        }
        Err(error) => {
            log::warn!("sync failed vault={vault_key}: {error}");
            fail(error)
        }
    }
}

/// Walk the vault and total bytes + file counts per sync scope.
fn collect_scope_sizes(dir: &Path) -> SyncScopeSizes {
    let mut sizes = SyncScopeSizes::default();
    let walker = walkdir::WalkDir::new(dir)
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
            .strip_prefix(dir)
            .ok()
            .and_then(|path| path.to_str())
            .map(|path| path.replace('\\', "/"))
        else {
            continue;
        };
        let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        match snapshot::scope_category(&rel) {
            Some("pdf") => {
                sizes.pdf += size;
                sizes.pdf_files += 1;
            }
            Some("source") => {
                sizes.source += size;
                sizes.source_files += 1;
            }
            Some("attachments") => {
                sizes.attachments += size;
                sizes.attachment_files += 1;
            }
            _ => {}
        }
    }
    sizes
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
    let result = blocking(move || Ok(collect_scope_sizes(&dir))).await;
    match result {
        Ok(sizes) => typed_ok(sizes),
        Err(error) => fail_msg(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_sync_interval_floor_is_one_minute() {
        let mut cfg = SyncBackendConfig {
            interval_minutes: 0,
            ..SyncBackendConfig::default()
        };
        assert_eq!(auto_sync_interval(&cfg), Duration::from_secs(60));
        cfg.interval_minutes = 1;
        assert_eq!(auto_sync_interval(&cfg), Duration::from_secs(60));
        cfg.interval_minutes = 30;
        assert_eq!(auto_sync_interval(&cfg), Duration::from_secs(30 * 60));
    }

    #[test]
    fn scope_sizes_reports_bytes_and_file_counts() {
        let dir = std::env::temp_dir().join(format!(
            "agentero-web-sync-scope-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("papers/a/source")).unwrap();
        std::fs::create_dir_all(dir.join("papers/a/attachments")).unwrap();
        std::fs::write(dir.join("papers/a/a.pdf"), b"%PDF-1.7").unwrap();
        std::fs::write(dir.join("papers/a/NOTES.md"), b"# notes").unwrap();
        std::fs::write(dir.join("papers/a/source/main.tex"), b"\\documentclass").unwrap();
        std::fs::write(dir.join("papers/a/attachments/sup.bin"), b"att").unwrap();
        std::fs::create_dir_all(dir.join(".agentero")).unwrap();
        std::fs::write(dir.join(".agentero/catalog.sqlite"), b"ignored").unwrap();

        let sizes = collect_scope_sizes(&dir);
        assert_eq!(sizes.pdf, 8);
        assert_eq!(sizes.pdf_files, 1);
        assert_eq!(sizes.source, 14);
        assert_eq!(sizes.source_files, 1);
        assert_eq!(sizes.attachments, 3);
        assert_eq!(sizes.attachment_files, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lease_is_exclusive_per_vault() {
        let service = SyncService::new(EventBus::new());
        assert!(service.try_begin("vault-a"));
        assert!(!service.try_begin("vault-a"), "second pass is rejected");
        assert!(service.try_begin("vault-b"));
        service.end("vault-a");
        assert!(service.try_begin("vault-a"));
    }
}
