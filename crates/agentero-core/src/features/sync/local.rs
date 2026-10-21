//! Local sync state under `.agentero/` (ignored by the vault watcher, so
//! writes here never loop back as file-change events).
//!
//! - `.agentero/vault.json` — durable vault identity (UUID)
//! - `.agentero/sync/base.json` — manifest of the last successful sync
//! - `.agentero/sync/state.json` — last sync time / version for status UI
//! - `.agentero/sync/pushed.jsonl` — blobs uploaded but not yet published

use crate::error::AppError;
use crate::features::sync::engine::SyncOutcome;
use crate::features::sync::snapshot::Manifest;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultIdentity {
    id: String,
    created_at: String,
}

/// Durable summary of the last successful pass (survives panel refresh and
/// server restarts). Shapes the `lastOutcome` field of `sync_get_status`.
///
/// `conflict_copies` keeps the engine's vault-relative conflict-copy file
/// paths (`SyncOutcome.conflictCopies` is `string[]` everywhere).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LastSyncOutcome {
    pub version: u64,
    pub uploaded: u64,
    pub downloaded: u64,
    pub deleted_local: u64,
    pub removed_remote: u64,
    #[serde(default)]
    pub conflict_copies: Vec<String>,
    /// RFC 3339 millisecond timestamp of when the pass finished.
    pub finished_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncMeta {
    #[serde(default)]
    pub last_sync_at: Option<String>,
    #[serde(default)]
    pub last_version: u64,
    /// Last failed pass's error message; cleared by the next success.
    /// Surfaced by the CLI `sync status` (the desktop reports it live via
    /// `sync:state` events instead) and by the Web `sync_get_status`.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Summary of the last successful pass; untouched by later failures.
    #[serde(default)]
    pub last_outcome: Option<LastSyncOutcome>,
}

/// Record a failed pass on top of the existing meta (keeps last_sync_at /
/// last_version from the last success). Best-effort: a broken state.json
/// must not mask the sync error itself.
pub fn record_error(vault: &Path, error: &str) {
    let mut meta = read_meta(vault);
    meta.last_error = Some(error.to_string());
    let _ = write_meta(vault, &meta);
}

/// Format a `SystemTime` as the canonical RFC 3339 millisecond timestamp
/// (scheduler next-run estimates share the `generatedAt` format).
pub fn rfc3339_millis_from_system_time(time: std::time::SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = time.into();
    datetime.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Persist the summary of a successful pass (keeps `last_sync_at` /
/// `last_version` written by the engine, clears `last_error`).
/// Best-effort: failing to write the summary must never fail the pass.
pub fn record_outcome(vault: &Path, outcome: &SyncOutcome) {
    let mut meta = read_meta(vault);
    meta.last_error = None;
    meta.last_outcome = Some(LastSyncOutcome {
        version: outcome.version,
        uploaded: outcome.uploaded as u64,
        downloaded: outcome.downloaded as u64,
        deleted_local: outcome.deleted_local as u64,
        removed_remote: outcome.removed_remote as u64,
        conflict_copies: outcome.conflict_copies.clone(),
        finished_at: crate::time::now_rfc3339_millis(),
    });
    if let Err(error) = write_meta(vault, &meta) {
        log::warn!(target: "agentero::sync", "record sync outcome: {error}");
    }
}

fn identity_path(vault: &Path) -> PathBuf {
    vault.join(".agentero").join("vault.json")
}

fn sync_dir(vault: &Path) -> PathBuf {
    vault.join(".agentero").join("sync")
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::fs::json_store(path, value)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Durable vault UUID, created on first use.
pub fn ensure_vault_id(vault: &Path) -> Result<String, AppError> {
    if let Some(identity) = read_json::<VaultIdentity>(&identity_path(vault)) {
        if !identity.id.trim().is_empty() {
            return Ok(identity.id);
        }
    }
    let identity = VaultIdentity {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: crate::time::now_rfc3339_millis(),
    };
    write_json(&identity_path(vault), &identity)?;
    Ok(identity.id)
}

/// Adopt a remote store's vault id (first join of an existing remote).
pub fn set_vault_id(vault: &Path, id: &str) -> Result<(), AppError> {
    write_json(
        &identity_path(vault),
        &VaultIdentity {
            id: id.to_string(),
            created_at: crate::time::now_rfc3339_millis(),
        },
    )
}

pub fn read_base(vault: &Path) -> Manifest {
    read_json(&sync_dir(vault).join("base.json")).unwrap_or_default()
}

pub fn write_base(vault: &Path, manifest: &Manifest) -> Result<(), AppError> {
    write_json(&sync_dir(vault).join("base.json"), manifest)
}

pub fn read_meta(vault: &Path) -> SyncMeta {
    read_json(&sync_dir(vault).join("state.json")).unwrap_or_default()
}

pub fn write_meta(vault: &Path, meta: &SyncMeta) -> Result<(), AppError> {
    write_json(&sync_dir(vault).join("state.json"), meta)
}

/// Forget local sync state (disconnect). Keeps the vault identity.
pub fn clear(vault: &Path) {
    let _ = fs::remove_dir_all(sync_dir(vault));
}

// ---- Pushed-blob log -------------------------------------------------------
//
// Blobs a pass uploaded but never got to publish, so the next pass can skip
// them instead of re-sending every byte to a server that ignores
// `If-None-Match` (Nutstore). Appended per blob rather than written once at the
// end: a rate-limit error, a quit or an aborted task must not lose the record.
// The first line pins the remote store, because skipping an upload a different
// store never received would publish a manifest referencing missing blobs.

fn pushed_path(vault: &Path) -> PathBuf {
    sync_dir(vault).join("pushed.jsonl")
}

fn pushed_header(store_id: &str) -> String {
    format!("#{store_id}")
}

/// Hashes already uploaded to `store_id`. A log left by a different store is
/// removed rather than kept: appending under a header that no longer matches
/// would strand it, and the records are worthless for the new store anyway.
pub fn read_pushed(vault: &Path, store_id: &str) -> HashSet<String> {
    let path = pushed_path(vault);
    let Ok(raw) = fs::read_to_string(&path) else {
        return HashSet::new();
    };
    let mut lines = raw.lines();
    if lines.next() == Some(pushed_header(store_id).as_str()) {
        return lines
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
    }
    let _ = fs::remove_file(&path);
    HashSet::new()
}

/// Record one uploaded blob. Best-effort: a lost record only costs a redundant
/// upload, so a write failure must not fail the pass it is bookkeeping for.
pub fn record_pushed(vault: &Path, store_id: &str, hash: &str) {
    let path = pushed_path(vault);
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        if file.metadata()?.len() == 0 {
            writeln!(file, "{}", pushed_header(store_id))?;
        }
        writeln!(file, "{hash}")
    })();
    if let Err(e) = result {
        log::warn!(target: "agentero::sync", "record pushed blob {hash}: {e}");
    }
}

/// Forget the log: a published manifest now covers those blobs, and it is the
/// authority every device consults.
pub fn clear_pushed(vault: &Path) {
    let _ = fs::remove_file(pushed_path(vault));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushed_log_appends_dedupes_and_is_scoped_to_one_store() {
        let vault =
            std::env::temp_dir().join(format!("agentero-sync-local-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&vault).unwrap();

        assert!(read_pushed(&vault, "store-a").is_empty());
        record_pushed(&vault, "store-a", "h1");
        record_pushed(&vault, "store-a", "h2");
        // The same hash again after a second aborted pass.
        record_pushed(&vault, "store-a", "h1");
        assert_eq!(
            read_pushed(&vault, "store-a"),
            HashSet::from(["h1".to_string(), "h2".to_string()])
        );

        // Another store inherits nothing, and recording for it retires the
        // old log so the two cannot end up interleaved under one header.
        assert!(read_pushed(&vault, "store-b").is_empty());
        record_pushed(&vault, "store-b", "h3");
        assert_eq!(
            read_pushed(&vault, "store-b"),
            HashSet::from(["h3".to_string()])
        );
        assert!(read_pushed(&vault, "store-a").is_empty());

        clear_pushed(&vault);
        assert!(read_pushed(&vault, "store-b").is_empty());

        let _ = fs::remove_dir_all(&vault);
    }

    #[test]
    fn outcome_is_persisted_and_cleared_by_the_next_success() {
        let vault =
            std::env::temp_dir().join(format!("agentero-sync-outcome-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&vault).unwrap();

        record_error(&vault, "previous failure");
        record_outcome(
            &vault,
            &SyncOutcome {
                version: 7,
                uploaded: 2,
                downloaded: 1,
                deleted_local: 0,
                removed_remote: 3,
                conflict_copies: vec![
                    "papers/attention/NOTES.conflict-2026.md".to_string(),
                    "notes/scratch.conflict-1.md".to_string(),
                ],
            },
        );
        let meta = read_meta(&vault);
        assert_eq!(meta.last_error, None, "success clears the failure");
        let outcome = meta.last_outcome.expect("outcome persisted");
        assert_eq!(outcome.version, 7);
        assert_eq!(outcome.uploaded, 2);
        assert_eq!(outcome.removed_remote, 3);
        assert!(!outcome.finished_at.is_empty());
        assert_eq!(
            outcome.conflict_copies,
            vec![
                "papers/attention/NOTES.conflict-2026.md".to_string(),
                "notes/scratch.conflict-1.md".to_string(),
            ]
        );

        // A later failure keeps the last successful outcome.
        record_error(&vault, "network down");
        let meta = read_meta(&vault);
        assert_eq!(meta.last_error.as_deref(), Some("network down"));
        assert_eq!(meta.last_outcome.expect("kept").version, 7);

        // The summary round-trips through state.json (panel refresh / restart).
        let roundtrip = read_json::<SyncMeta>(&sync_dir(&vault).join("state.json")).expect("json");
        assert_eq!(roundtrip.last_outcome.expect("roundtrip").uploaded, 2);
        let _ = fs::remove_dir_all(&vault);
    }

    #[test]
    fn disconnect_also_forgets_the_pushed_log() {
        let vault =
            std::env::temp_dir().join(format!("agentero-sync-local-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&vault).unwrap();
        record_pushed(&vault, "store-a", "h1");
        clear(&vault);
        assert!(read_pushed(&vault, "store-a").is_empty());
        let _ = fs::remove_dir_all(&vault);
    }
}
