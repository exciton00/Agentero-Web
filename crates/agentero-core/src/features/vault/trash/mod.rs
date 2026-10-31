//! Vault-local recycle bin.
//!
//! Deleting an item moves it into `.agentero/.trash/<batchId>/` together with a
//! `manifest.json` that records each item's original vault-relative path and a
//! snapshot of the catalog rows removed for it. This makes a delete fully
//! undoable — files move back and catalog rows are re-inserted — without
//! depending on platform-specific OS trash restore.

use crate::error::AppError;
use crate::features::catalog::papers::{self, PaperRecord};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Vault-relative location of the recycle bin (hidden from the file tree,
/// which skips dot-entries).
const TRASH_REL: &str = ".agentero/.trash";

/// One manifest entry. Field names stay camelCase so existing `manifest.json`
/// files (and manifests written by the desktop app) still deserialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrashItem {
    /// Original vault-relative path the item lived at.
    rel: String,
    /// Basename of the stored copy inside the batch directory.
    stored: String,
    /// Catalog rows removed for this item (empty when not under `papers/`).
    #[serde(default)]
    catalog_rows: Vec<PaperRecord>,
    /// Durable move intent: a retry can tell "already moved back" from
    /// "never moved" without guessing from the filesystem alone.
    #[serde(default)]
    phase: TrashPhase,
}

/// Restore phases. Old manifests already describe trashed files, hence the
/// `Trashed` default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum TrashPhase {
    Prepared,
    #[default]
    Trashed,
    Restoring,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreAction {
    /// Stored copy still in the bin, original path free: move it back.
    Move,
    /// A previous attempt already moved the files; only the catalog is left.
    Finish,
    /// A prepared item was never moved: nothing to restore.
    Unmoved,
}

/// Decide how a restore attempt for one item proceeds.
///
/// `stored` = the copy is still in the batch directory, `target` = the original
/// vault path currently exists.
fn restore_action(item: &TrashItem, stored: bool, target: bool) -> Result<RestoreAction, AppError> {
    match (stored, target, item.phase) {
        (true, false, _) => Ok(RestoreAction::Move),
        (false, true, TrashPhase::Restoring) => Ok(RestoreAction::Finish),
        (false, true, TrashPhase::Prepared) => Ok(RestoreAction::Unmoved),
        (_, true, _) => Err(AppError::message(format!(
            "cannot restore: '{}' already exists",
            item.rel
        ))),
        _ => Err(AppError::message(format!(
            "trash content missing: {}",
            item.rel
        ))),
    }
}

/// Sidecar location relative to the stored item, including organization folders.
fn restore_sidecar_path(
    item: &TrashItem,
    row: &PaperRecord,
) -> Result<std::path::PathBuf, AppError> {
    let item_path = crate::fs::normalize_rel_separators(&item.rel);
    let row_path = crate::fs::normalize_rel_separators(&row.path);
    let suffix = Path::new(&row_path)
        .strip_prefix(&item_path)
        .map_err(|_| AppError::message("catalog snapshot is outside its trash item"))?;
    Ok(suffix.join(crate::features::catalog::sidecar::SIDECAR_FILE))
}

/// Re-insert removed catalog rows, but never overwrite metadata observed after
/// the interrupted restore (watcher indexing, user edit, sync pull).
fn restore_catalog(vault: &Path, item: &TrashItem) -> Result<(), AppError> {
    for row in &item.catalog_rows {
        if papers::get_by_path(vault, &row.path)?.is_none() {
            papers::upsert_paper(vault, row)?;
        }
    }
    Ok(())
}

/// Recycle-bin batch manifest (`manifest.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrashManifest {
    batch_id: String,
    created_at: String,
    items: Vec<TrashItem>,
}

/// Result of a trash operation returned to the UI.
#[derive(Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct TrashResult {
    /// Batch id used to undo (restore) the whole delete.
    pub batch_id: String,
    /// Number of items actually moved into the recycle bin.
    pub count: usize,
    /// Vault-relative paths actually moved (subset of the requested rels).
    pub rels: Vec<String>,
}

/// One item in the recycle bin (flattened across batches) for the UI list.
#[derive(Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntry {
    /// Stable id: "{batchId}::{stored}".
    pub id: String,
    pub batch_id: String,
    pub stored: String,
    /// Original vault-relative path.
    pub rel: String,
    /// Basename for display.
    pub name: String,
    /// Paper title when the item snapshots exactly one catalog row (a paper
    /// directory); `None` for plain files or folders holding several papers.
    pub title: Option<String>,
    pub deleted_at: String,
    pub is_dir: bool,
}

/// Display title for a trashed item: the catalog title when the snapshot holds
/// a single paper row, otherwise `None` (caller falls back to the basename).
pub fn snapshot_title(rows: &[PaperRecord]) -> Option<String> {
    if rows.len() != 1 {
        return None;
    }
    let title = rows[0].title.trim();
    (!title.is_empty()).then(|| title.to_string())
}

fn is_under_papers(rel: &str) -> bool {
    rel == "papers" || rel.starts_with("papers/")
}

fn validate_batch_id(batch_id: &str) -> Result<(), AppError> {
    if batch_id.is_empty()
        || batch_id.contains('/')
        || batch_id.contains('\\')
        || batch_id.contains("..")
    {
        return Err(AppError::message("invalid batch id"));
    }
    Ok(())
}

fn read_manifest(batch_dir: &Path) -> Result<TrashManifest, AppError> {
    let raw = fs::read_to_string(batch_dir.join("manifest.json"))
        .map_err(|_| AppError::message("trash batch not found"))?;
    serde_json::from_str(&raw).map_err(|e| AppError::message(e.to_string()))
}

fn write_manifest(batch_dir: &Path, manifest: &TrashManifest) -> Result<(), AppError> {
    let json =
        serde_json::to_string_pretty(manifest).map_err(|e| AppError::message(e.to_string()))?;
    fs::write(batch_dir.join("manifest.json"), json)?;
    Ok(())
}

/// Move the given vault-relative paths into a new recycle-bin batch.
///
/// For anything under `papers/`, catalog rows are snapshotted then removed so
/// the Library stays consistent; both are restored on [`restore_batch`].
/// Skips empty / traversing / `.agentero` / `papers` root / missing paths.
pub fn trash_paths(vault_root: &Path, rels: &[String]) -> Result<TrashResult, AppError> {
    crate::fs::ensure_vault_dir(vault_root)?;
    let now = chrono::Utc::now();
    let batch_id = format!(
        "{}-{}",
        now.format("%Y%m%dT%H%M%S"),
        now.timestamp_subsec_nanos()
    );
    let batch_dir = vault_root.join(TRASH_REL).join(&batch_id);

    let mut items: Vec<TrashItem> = Vec::new();
    for (i, raw) in rels.iter().enumerate() {
        let rel = crate::fs::normalize_rel_separators(raw);
        if rel.is_empty() || rel == "papers" || rel.contains("..") {
            continue;
        }
        if rel.starts_with(".agentero") {
            continue; // never trash the app's private dir
        }
        let abs = vault_root.join(&rel);
        if !abs.exists() {
            continue;
        }
        // Index prefix avoids basename collisions within one batch.
        let base = Path::new(&rel)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("item")
            .to_string();
        let stored = format!("{i}__{base}");
        // Snapshot catalog rows first (read-only), but keep them until the move
        // succeeds — otherwise a failed rename (e.g. a file open on Windows)
        // would orphan the catalog and hide the paper from the Library.
        let catalog_rows = if is_under_papers(&rel) {
            papers::list_under_path(vault_root, &rel).unwrap_or_default()
        } else {
            Vec::new()
        };
        if fs::create_dir_all(&batch_dir).is_err() {
            continue;
        }
        if fs::rename(&abs, batch_dir.join(&stored)).is_err() {
            // Leave the item in place and the catalog untouched.
            continue;
        }
        if is_under_papers(&rel) {
            let _ = papers::delete_under_path(vault_root, &rel);
        }
        items.push(TrashItem {
            rel,
            stored,
            catalog_rows,
            phase: TrashPhase::Trashed,
        });
    }

    if items.is_empty() {
        // Nothing moved — drop the (possibly created) empty batch dir.
        let _ = fs::remove_dir(&batch_dir);
        return Ok(TrashResult {
            batch_id,
            count: 0,
            rels: Vec::new(),
        });
    }

    let rels: Vec<String> = items.iter().map(|item| item.rel.clone()).collect();
    let manifest = TrashManifest {
        batch_id: batch_id.clone(),
        created_at: crate::time::now_rfc3339_millis(),
        items,
    };
    let json =
        serde_json::to_string_pretty(&manifest).map_err(|e| AppError::message(e.to_string()))?;
    fs::write(batch_dir.join("manifest.json"), json)?;

    Ok(TrashResult {
        batch_id: manifest.batch_id,
        count: manifest.items.len(),
        rels,
    })
}

/// Result of a delete request (trash or permanent).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub permanent: bool,
    /// Present for recycle-bin deletes; `None` for permanent deletes.
    pub batch_id: Option<String>,
    pub count: usize,
    pub paths: Vec<String>,
    /// Catalog rows removed under `papers/` (permanent deletes only).
    pub removed_rows: usize,
}

/// Delete vault paths either into the recycle bin (default, undoable) or
/// permanently. Shared by CLI `paper delete` and MCP `file_delete`.
pub fn delete_paths(
    vault_root: &Path,
    rels: &[String],
    permanent: bool,
) -> Result<DeleteResult, AppError> {
    if !permanent {
        let result = trash_paths(vault_root, rels)?;
        return Ok(DeleteResult {
            permanent: false,
            batch_id: Some(result.batch_id),
            count: result.count,
            paths: result.rels,
            removed_rows: 0,
        });
    }

    crate::fs::ensure_vault_dir(vault_root)?;
    let root = vault_root.canonicalize()?;
    let mut clean: Vec<(String, std::path::PathBuf, bool)> = Vec::new();
    for raw in rels {
        let rel = crate::fs::normalize_rel_separators(raw);
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            continue;
        }
        if rel == "papers"
            || rel == ".agentero"
            || rel.starts_with(".agentero/")
            || rel.split('/').any(|s| s == "..")
        {
            return Err(AppError::message(format!(
                "cannot permanently delete protected path: {rel}"
            )));
        }
        let abs = vault_root.join(rel);
        let meta = fs::symlink_metadata(&abs)
            .map_err(|_| AppError::message(format!("path not found: {rel}")))?;
        let resolved = abs.canonicalize()?;
        if !resolved.starts_with(&root) {
            return Err(AppError::message(format!("path escapes vault root: {rel}")));
        }
        clean.push((rel.to_string(), abs, meta.is_dir()));
    }
    if clean.is_empty() {
        return Err(AppError::message("paths is required"));
    }

    let mut removed_rows = 0usize;
    let mut paths = Vec::new();
    for (rel, abs, is_dir) in clean {
        removed_rows += papers::delete_under_path(vault_root, &rel)?;
        if is_dir {
            fs::remove_dir_all(&abs)?;
        } else {
            fs::remove_file(&abs)?;
        }
        paths.push(rel);
    }
    Ok(DeleteResult {
        permanent: true,
        batch_id: None,
        count: paths.len(),
        paths,
        removed_rows,
    })
}

/// Restore a recycle-bin batch: move files back and re-insert catalog rows.
///
/// Retryable: the per-item phase is persisted before the file move, so an
/// interrupted restore (crash, catalog failure, killed process) can be re-run
/// and only finishes what is left. Aborts without changes if any original path
/// was re-created by something else, so a restore never clobbers newer content.
pub fn restore_batch(vault_root: &Path, batch_id: &str) -> Result<usize, AppError> {
    crate::fs::ensure_vault_dir(vault_root)?;
    validate_batch_id(batch_id)?;
    let batch_dir = vault_root.join(TRASH_REL).join(batch_id);
    let mut manifest = read_manifest(&batch_dir)?;
    let count = manifest.items.len();
    // Preflight all targets before changing any files.
    for item in &manifest.items {
        restore_action(
            item,
            batch_dir.join(&item.stored).try_exists()?,
            vault_root.join(&item.rel).try_exists()?,
        )?;
    }
    for index in 0..count {
        restore_local_entry(vault_root, &batch_dir, &mut manifest, index)?;
    }
    fs::remove_dir_all(&batch_dir)?;
    Ok(count)
}

/// Restore one manifest entry, resumable from its persisted phase.
fn restore_local_entry(
    vault_root: &Path,
    batch_dir: &Path,
    manifest: &mut TrashManifest,
    index: usize,
) -> Result<(), AppError> {
    let item = &manifest.items[index];
    let target = vault_root.join(&item.rel);
    match restore_action(
        item,
        batch_dir.join(&item.stored).try_exists()?,
        target.try_exists()?,
    )? {
        RestoreAction::Unmoved => return Ok(()),
        RestoreAction::Move => {
            manifest.items[index].phase = TrashPhase::Restoring;
            write_manifest(batch_dir, manifest)?;
            let item = &manifest.items[index];
            // Project the snapshot metadata into the restored folder *before*
            // the move: a watcher may index the directory as soon as it
            // reappears, and without the sidecar it would insert a minimal row
            // and lose the paper's metadata.
            for row in &item.catalog_rows {
                let sidecar = batch_dir
                    .join(&item.stored)
                    .join(restore_sidecar_path(item, row)?);
                // The catalog may still contain a child whose folder was
                // removed outside the app; never recreate those directories.
                if !sidecar.parent().expect("sidecar parent").try_exists()? {
                    continue;
                }
                crate::fs::atomic_write(&sidecar, &serde_json::to_vec_pretty(row)?)?;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(batch_dir.join(&manifest.items[index].stored), &target)?;
        }
        RestoreAction::Finish => {}
    }
    restore_catalog(vault_root, &manifest.items[index])
}

/// List every item currently in the recycle bin, newest batch first.
pub fn list_trash(vault_root: &Path) -> Result<Vec<TrashEntry>, AppError> {
    let root = vault_root.join(TRASH_REL);
    let mut out: Vec<TrashEntry> = Vec::new();
    let Ok(read) = fs::read_dir(&root) else {
        return Ok(out);
    };
    for ent in read.flatten() {
        let batch_dir = ent.path();
        if !batch_dir.is_dir() {
            continue;
        }
        let Ok(manifest) = read_manifest(&batch_dir) else {
            continue;
        };
        for item in &manifest.items {
            // A prepared-but-unmoved entry is not visible content (the file is
            // still at its live path); only its intent is recorded.
            if item.phase == TrashPhase::Prepared && !batch_dir.join(&item.stored).try_exists()? {
                continue;
            }
            let is_dir = batch_dir.join(&item.stored).is_dir();
            let name = Path::new(&item.rel)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(item.rel.as_str())
                .to_string();
            out.push(TrashEntry {
                id: format!("{}::{}", manifest.batch_id, item.stored),
                batch_id: manifest.batch_id.clone(),
                stored: item.stored.clone(),
                rel: item.rel.clone(),
                name,
                title: snapshot_title(&item.catalog_rows),
                deleted_at: manifest.created_at.clone(),
                is_dir,
            });
        }
    }
    out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    Ok(out)
}

/// Restore a single recycle-bin item to its original path (files + catalog rows).
/// Retryable like [`restore_batch`]. Returns the restored rel path.
pub fn restore_item(vault_root: &Path, batch_id: &str, stored: &str) -> Result<String, AppError> {
    crate::fs::ensure_vault_dir(vault_root)?;
    validate_batch_id(batch_id)?;
    let batch_dir = vault_root.join(TRASH_REL).join(batch_id);
    let mut manifest = read_manifest(&batch_dir)?;
    let idx = manifest
        .items
        .iter()
        .position(|i| i.stored == stored)
        .ok_or_else(|| AppError::message("trash item not found"))?;

    restore_local_entry(vault_root, &batch_dir, &mut manifest, idx)?;
    let item = manifest.items.remove(idx);
    if manifest.items.is_empty() {
        fs::remove_dir_all(&batch_dir)?;
    } else {
        write_manifest(&batch_dir, &manifest)?;
    }
    Ok(item.rel)
}

/// Permanently delete a single recycle-bin item.
pub fn purge_item(vault_root: &Path, batch_id: &str, stored: &str) -> Result<(), AppError> {
    crate::fs::ensure_vault_dir(vault_root)?;
    validate_batch_id(batch_id)?;
    let batch_dir = vault_root.join(TRASH_REL).join(batch_id);
    let mut manifest = read_manifest(&batch_dir)?;
    let idx = manifest
        .items
        .iter()
        .position(|i| i.stored == stored)
        .ok_or_else(|| AppError::message("trash item not found"))?;
    let item = manifest.items.remove(idx);
    let stored_path = batch_dir.join(&item.stored);
    if stored_path.is_dir() {
        let _ = fs::remove_dir_all(&stored_path);
    } else {
        let _ = fs::remove_file(&stored_path);
    }
    if manifest.items.is_empty() {
        let _ = fs::remove_dir_all(&batch_dir);
    } else {
        write_manifest(&batch_dir, &manifest)?;
    }
    Ok(())
}

/// Empty the entire recycle bin (permanent).
pub fn purge_all(vault_root: &Path) -> Result<(), AppError> {
    let root = vault_root.join(TRASH_REL);
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn sample_record(path: &str, id: &str) -> PaperRecord {
        PaperRecord {
            path: path.into(),
            id: id.into(),
            paper_type: papers::PaperKind::Doi,
            title: "T".into(),
            authors: vec![],
            creators: None,
            year: None,
            date: None,
            abstract_text: None,
            tags: vec![crate::features::catalog::papers::PaperTag::new("NLP")],
            arxiv_id: None,
            doi: None,
            isbn: None,
            issn: None,
            pmid: None,
            publication: None,
            volume: None,
            issue: None,
            pages: None,
            publisher: None,
            place: None,
            series: None,
            language: None,
            pdf_url: None,
            html_url: None,
            source_url: None,
            code_repo: None,
            body_source: None,
            body_quality: None,
            bibtex_key: None,
            citation_count: None,
            zotero_item_type: None,
            meta_source: None,
            extra: None,
            summary: None,
            status: "completed".into(),
            is_read: false,
            zotero_item_id: None,
            zotero_last_synced: None,
            added_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    #[test]
    fn trash_and_restore_roundtrip() {
        let dir = env::temp_dir().join(format!("agentero-trash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paper_dir = dir.join("papers").join("x");
        fs::create_dir_all(&paper_dir).unwrap();
        fs::write(paper_dir.join("NOTES.md"), "# hi\n").unwrap();
        papers::upsert_paper(&dir, &sample_record("papers/x", "x")).unwrap();

        // Trash: files move out, catalog row gone.
        let res = trash_paths(&dir, &["papers/x".to_string()]).unwrap();
        assert_eq!(res.count, 1);
        assert!(!paper_dir.exists());
        assert!(papers::get_by_path(&dir, "papers/x").unwrap().is_none());

        // Restore: files back, catalog row re-inserted.
        let restored = restore_batch(&dir, &res.batch_id).unwrap();
        assert_eq!(restored, 1);
        assert!(paper_dir.join("NOTES.md").exists());
        assert!(papers::get_by_path(&dir, "papers/x").unwrap().is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_reports_paper_title() {
        let dir = env::temp_dir().join(format!("agentero-trash-title-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paper_dir = dir.join("papers").join("2501.00001");
        fs::create_dir_all(&paper_dir).unwrap();
        fs::write(paper_dir.join("NOTES.md"), "# hi\n").unwrap();
        papers::upsert_paper(&dir, &sample_record("papers/2501.00001", "2501.00001")).unwrap();
        let notes = dir.join("notes");
        fs::create_dir_all(&notes).unwrap();
        fs::write(notes.join("a.md"), "a\n").unwrap();

        let res = trash_paths(
            &dir,
            &["papers/2501.00001".to_string(), "notes/a.md".to_string()],
        )
        .unwrap();
        assert_eq!(res.count, 2);

        let items = list_trash(&dir).unwrap();
        let paper = items.iter().find(|e| e.rel == "papers/2501.00001").unwrap();
        // Paper entries surface the snapshotted catalog title for display.
        assert_eq!(paper.title.as_deref(), Some("T"));
        let note = items.iter().find(|e| e.rel == "notes/a.md").unwrap();
        assert!(note.title.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_aborts_when_path_reappears() {
        let dir = env::temp_dir().join(format!("agentero-trash-conflict-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let note = dir.join("notes");
        fs::create_dir_all(&note).unwrap();
        fs::write(note.join("a.md"), "one\n").unwrap();

        let res = trash_paths(&dir, &["notes/a.md".to_string()]).unwrap();
        assert_eq!(res.count, 1);
        // A new file reoccupies the path.
        fs::write(note.join("a.md"), "two\n").unwrap();

        assert!(restore_batch(&dir, &res.batch_id).is_err());
        // The reoccupying file is untouched.
        assert_eq!(fs::read_to_string(note.join("a.md")).unwrap(), "two\n");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_paths_supports_trash_and_permanent_modes() {
        let dir = env::temp_dir().join(format!("agentero-delete-mode-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("notes")).unwrap();
        fs::write(dir.join("notes/a.md"), "a\n").unwrap();

        let trashed = delete_paths(&dir, &["notes/a.md".into()], false).unwrap();
        assert!(!trashed.permanent);
        assert!(trashed.batch_id.is_some());
        assert_eq!(trashed.count, 1);
        assert!(restore_batch(&dir, trashed.batch_id.as_deref().unwrap()).is_ok());

        let deleted = delete_paths(&dir, &["notes/a.md".into()], true).unwrap();
        assert!(deleted.permanent);
        assert!(deleted.batch_id.is_none());
        assert!(!dir.join("notes/a.md").exists());
        assert!(delete_paths(&dir, &["papers".into()], true).is_err());
        assert!(delete_paths(&dir, &[".agentero/x".into()], true).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_restore_and_purge_items() {
        let dir = env::temp_dir().join(format!("agentero-trash-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let notes = dir.join("notes");
        fs::create_dir_all(&notes).unwrap();
        fs::write(notes.join("a.md"), "a\n").unwrap();
        fs::write(notes.join("b.md"), "b\n").unwrap();

        // One batch with two items.
        let res = trash_paths(&dir, &["notes/a.md".to_string(), "notes/b.md".to_string()]).unwrap();
        assert_eq!(res.count, 2);
        assert_eq!(list_trash(&dir).unwrap().len(), 2);

        // Restore one item; the other stays in the bin.
        let a = list_trash(&dir)
            .unwrap()
            .into_iter()
            .find(|e| e.rel == "notes/a.md")
            .unwrap();
        restore_item(&dir, &a.batch_id, &a.stored).unwrap();
        assert!(notes.join("a.md").exists());
        assert_eq!(list_trash(&dir).unwrap().len(), 1);

        // Purge the remaining item; the bin becomes empty.
        let b = list_trash(&dir).unwrap().into_iter().next().unwrap();
        purge_item(&dir, &b.batch_id, &b.stored).unwrap();
        assert!(list_trash(&dir).unwrap().is_empty());
        assert!(!notes.join("b.md").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    /// A paper directory whose metadata.json is missing (e.g. a failed
    /// projection) must get the snapshot metadata written back *before* the
    /// rename, so a watcher indexing the restored folder cannot lose it.
    #[test]
    fn restore_projects_metadata_before_move() {
        let root = tempfile::tempdir().unwrap();
        let paper_dir = root.path().join("papers/x");
        fs::create_dir_all(&paper_dir).unwrap();
        fs::write(paper_dir.join("NOTES.md"), "# hi\n").unwrap();
        papers::upsert_paper(root.path(), &sample_record("papers/x", "x")).unwrap();
        fs::remove_file(paper_dir.join("metadata.json")).unwrap();

        let res = trash_paths(root.path(), &["papers/x".into()]).unwrap();
        restore_batch(root.path(), &res.batch_id).unwrap();

        let sidecar = paper_dir.join(crate::features::catalog::sidecar::SIDECAR_FILE);
        assert!(sidecar.is_file(), "restore must project metadata from the manifest");
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&sidecar).unwrap()).unwrap();
        assert_eq!(value["title"], "T");
        assert_eq!(
            papers::get_by_path(root.path(), "papers/x").unwrap().unwrap().title,
            "T"
        );
    }

    /// Crash after the files moved but before the catalog restore: the retry
    /// must finish the job instead of aborting on the reappeared path.
    #[test]
    fn restore_retries_after_interrupted_move() {
        let root = tempfile::tempdir().unwrap();
        let paper_dir = root.path().join("papers/x");
        fs::create_dir_all(&paper_dir).unwrap();
        fs::write(paper_dir.join("NOTES.md"), "# hi\n").unwrap();
        papers::upsert_paper(root.path(), &sample_record("papers/x", "x")).unwrap();
        let res = trash_paths(root.path(), &["papers/x".into()]).unwrap();
        assert!(papers::get_by_path(root.path(), "papers/x").unwrap().is_none());

        // Simulate the crash: phase=restoring persisted, file rename done,
        // catalog rows not restored yet.
        let batch_dir = root.path().join(TRASH_REL).join(&res.batch_id);
        let mut manifest = read_manifest(&batch_dir).unwrap();
        manifest.items[0].phase = TrashPhase::Restoring;
        write_manifest(&batch_dir, &manifest).unwrap();
        let stored = batch_dir.join(&manifest.items[0].stored);
        fs::rename(stored, &paper_dir).unwrap();

        let restored = restore_batch(root.path(), &res.batch_id).unwrap();
        assert_eq!(restored, 1);
        assert!(paper_dir.join("NOTES.md").exists());
        assert_eq!(
            papers::get_by_path(root.path(), "papers/x").unwrap().unwrap().title,
            "T"
        );
        assert!(list_trash(root.path()).unwrap().is_empty());
    }

    /// Catalog failure after the file move: re-running the restore finishes the
    /// missing catalog work (the old pre-check used to abort forever here).
    #[test]
    fn restore_retries_after_catalog_failure() {
        let root = tempfile::tempdir().unwrap();
        let paper_dir = root.path().join("papers/x");
        fs::create_dir_all(&paper_dir).unwrap();
        fs::write(paper_dir.join("NOTES.md"), "# hi\n").unwrap();
        papers::upsert_paper(root.path(), &sample_record("papers/x", "x")).unwrap();
        let res = trash_paths(root.path(), &["papers/x".into()]).unwrap();

        crate::features::catalog::with_catalog(root.path(), |conn| {
            conn.execute_batch(
                "CREATE TRIGGER fail_restore BEFORE INSERT ON papers
                 BEGIN SELECT RAISE(FAIL, 'injected'); END;",
            )?;
            Ok(())
        })
        .unwrap();
        let error = restore_batch(root.path(), &res.batch_id).unwrap_err();
        assert!(error.to_string().contains("injected"), "{error}");
        // Files are back already; only the catalog restore failed.
        assert!(paper_dir.join("NOTES.md").exists());

        crate::features::catalog::with_catalog(root.path(), |conn| {
            conn.execute_batch("DROP TRIGGER fail_restore")?;
            Ok(())
        })
        .unwrap();
        restore_batch(root.path(), &res.batch_id).unwrap();
        assert_eq!(
            papers::get_by_path(root.path(), "papers/x").unwrap().unwrap().title,
            "T"
        );
        assert!(list_trash(root.path()).unwrap().is_empty());
    }

    /// A retry must not roll metadata back to the trash snapshot when the
    /// catalog meanwhile holds a newer row (watcher index or user edit).
    #[test]
    fn restore_preserves_newer_metadata() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("papers/x")).unwrap();
        papers::upsert_paper(root.path(), &sample_record("papers/x", "x")).unwrap();
        let res = trash_paths(root.path(), &["papers/x".into()]).unwrap();

        let mut newer = sample_record("papers/x", "x");
        newer.title = "Latest".into();
        newer.updated_at = "2099-01-01T00:00:00.000Z".into();
        papers::upsert_paper(root.path(), &newer).unwrap();

        restore_batch(root.path(), &res.batch_id).unwrap();
        assert_eq!(
            papers::get_by_path(root.path(), "papers/x").unwrap().unwrap().title,
            "Latest"
        );
    }

    /// Catalog snapshot rows for children removed outside the app are restored
    /// as rows only; the restore must not recreate their directories.
    #[test]
    fn restore_parent_skips_orphan_catalog_rows_without_creating_directories() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("papers/group")).unwrap();
        fs::write(root.path().join("papers/group/note.md"), "keep").unwrap();
        papers::upsert_paper(
            root.path(),
            &sample_record("papers/group/missing", "missing"),
        )
        .unwrap();
        let res = trash_paths(root.path(), &["papers/group".into()]).unwrap();
        restore_batch(root.path(), &res.batch_id).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("papers/group/note.md")).unwrap(),
            "keep"
        );
        assert!(!root.path().join("papers/group/missing").exists());
    }
}
