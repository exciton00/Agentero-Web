//! Vault tag index cache: `<vault>/.agentero/tag-index.json`.
//!
//! The catalog is authoritative; this hidden file is a derived, rebuildable
//! cache so the sidebar's "expand by tag" tree and the Library tag filter get
//! one cheap read instead of walking the catalog on every render.
//!
//! Maintenance is lazy: [`read_tag_index`] rebuilds when the file is missing,
//! carries another schema `version`, or when the catalog fingerprint
//! (`COUNT(*)` + `MAX(updated_at)`) changed. Every catalog write (tags, meta,
//! import, trash, move/rename) bumps one of the two, so a stale cache cannot
//! survive a write. Handlers on hot write paths may additionally call
//! [`ensure_tag_index_best_effort`] to refresh eagerly.
//!
//! Internal tags (`@zotero:` / `@arxiv:`) are filtered out everywhere here:
//! they are provenance markers, not user taxonomies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::features::paper::catalog::papers::{self, PaperTag};
use crate::fs::json_store;
use crate::time::now_rfc3339_millis;

/// Schema version of the hidden cache file; bump on incompatible changes.
///
/// v2: `#`-prefixed reserved tags are filtered out (see
/// `papers::is_internal_tag_name`), so v1 caches must rebuild.
pub const TAG_INDEX_VERSION: u32 = 2;

/// Vault-relative location of the cache file (inside the hidden `.agentero/`).
pub const TAG_INDEX_REL: &str = ".agentero/tag-index.json";

/// One paper row in the hidden cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagIndexPaper {
    /// Vault-relative paper folder path (primary key).
    pub path: String,
    pub id: String,
    pub title: String,
    /// User tags only (internal tags already removed).
    #[serde(default)]
    pub tags: Vec<PaperTag>,
}

/// Hidden cache file shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagIndexFile {
    pub version: u32,
    pub generated_at: String,
    /// Catalog fingerprint used for staleness detection: row count.
    #[serde(default)]
    pub catalog_count: u64,
    /// Catalog fingerprint used for staleness detection: newest `updated_at`.
    #[serde(default)]
    pub catalog_updated_at: String,
    /// Catalog fingerprint: total `tags_json` bytes (catches tag edits that
    /// share an `updated_at` millisecond).
    #[serde(default)]
    pub catalog_tags_bytes: u64,
    /// Catalog fingerprint: total `path` bytes (catches same-millisecond moves).
    #[serde(default)]
    pub catalog_paths_bytes: u64,
    pub papers: Vec<TagIndexPaper>,
}

/// One tag with its vault-wide paper count (RPC projection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagIndexEntry {
    pub name: String,
    pub color: Option<String>,
    pub count: usize,
}

/// Per-paper tag projection (RPC shape).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagIndexPaperView {
    pub path: String,
    pub tags: Vec<PaperTag>,
}

/// `tags_index_read` response shape.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagIndexView {
    pub generated_at: String,
    pub tags: Vec<TagIndexEntry>,
    pub papers: Vec<TagIndexPaperView>,
    /// Papers whose user tag list is empty (sidebar "untagged" filter source).
    pub untagged: Vec<String>,
}

/// Absolute path of the hidden cache file.
pub fn tag_index_path(vault_root: &Path) -> PathBuf {
    vault_root.join(".agentero").join("tag-index.json")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogFingerprint {
    count: u64,
    updated_at: String,
    tags_bytes: u64,
    paths_bytes: u64,
}

fn catalog_fingerprint(vault_root: &Path) -> Result<CatalogFingerprint, AppError> {
    let row = crate::features::paper::catalog::with_catalog(vault_root, |conn| {
        let row = conn.query_row(
            "SELECT COUNT(*), COALESCE(MAX(updated_at), ''), \
                    COALESCE(SUM(LENGTH(tags_json)), 0), COALESCE(SUM(LENGTH(path)), 0) \
             FROM papers",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        Ok(row)
    })?;
    Ok(CatalogFingerprint {
        count: row.0.max(0) as u64,
        updated_at: row.1,
        tags_bytes: row.2.max(0) as u64,
        paths_bytes: row.3.max(0) as u64,
    })
}

/// Read the cache file when present and schema-compatible.
fn read_cached_file(vault_root: &Path) -> Option<TagIndexFile> {
    let path = tag_index_path(vault_root);
    let raw = std::fs::read_to_string(path).ok()?;
    let file: TagIndexFile = serde_json::from_str(&raw).ok()?;
    (file.version == TAG_INDEX_VERSION).then_some(file)
}

/// Rebuild the index from the catalog without touching the hidden file.
pub fn build_tag_index(vault_root: &Path) -> Result<TagIndexFile, AppError> {
    let records = papers::list_all(vault_root)?;
    let fingerprint = catalog_fingerprint(vault_root)?;
    let mut papers: Vec<TagIndexPaper> = records
        .into_iter()
        .filter(|record| !record.path.trim().is_empty())
        .map(|record| {
            let tags: Vec<PaperTag> = papers::normalize_tags(&record.tags)
                .into_iter()
                .filter(|tag| !papers::is_internal_tag_name(&tag.name))
                .collect();
            TagIndexPaper {
                path: record.path,
                id: record.id,
                title: record.title,
                tags,
            }
        })
        .collect();
    papers.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(TagIndexFile {
        version: TAG_INDEX_VERSION,
        generated_at: now_rfc3339_millis(),
        catalog_count: fingerprint.count,
        catalog_updated_at: fingerprint.updated_at,
        catalog_tags_bytes: fingerprint.tags_bytes,
        catalog_paths_bytes: fingerprint.paths_bytes,
        papers,
    })
}

/// Return a fresh-enough index, rebuilding and persisting it when stale.
pub fn ensure_tag_index(vault_root: &Path) -> Result<TagIndexFile, AppError> {
    crate::fs::ensure_vault_dir(vault_root)?;
    let fingerprint = catalog_fingerprint(vault_root)?;
    if let Some(file) = read_cached_file(vault_root) {
        if file.catalog_count == fingerprint.count
            && file.catalog_updated_at == fingerprint.updated_at
            && file.catalog_tags_bytes == fingerprint.tags_bytes
            && file.catalog_paths_bytes == fingerprint.paths_bytes
            && tag_index_path(vault_root).is_file()
        {
            return Ok(file);
        }
    }
    let file = build_tag_index(vault_root)?;
    write_tag_index(vault_root, &file)?;
    Ok(file)
}

/// Persist the hidden cache file (creates `.agentero/` when needed).
pub fn write_tag_index(vault_root: &Path, file: &TagIndexFile) -> Result<(), AppError> {
    let path = tag_index_path(vault_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AppError::message(format!("create tag index dir {}: {error}", parent.display()))
        })?;
    }
    json_store(&path, file)
}

/// Project the cached file into the `tags_index_read` wire shape.
pub fn tag_index_view(file: &TagIndexFile) -> TagIndexView {
    // key = lowercase tag name → (display name, color, count)
    let mut tags: BTreeMap<String, (String, Option<String>, usize)> = BTreeMap::new();
    let mut papers = Vec::with_capacity(file.papers.len());
    let mut untagged = Vec::new();
    for paper in &file.papers {
        if paper.tags.is_empty() {
            untagged.push(paper.path.clone());
        }
        for tag in &paper.tags {
            let key = tag.name.to_ascii_lowercase();
            let color = tag
                .color
                .as_deref()
                .map(str::trim)
                .filter(|color| !color.is_empty())
                .map(str::to_string);
            tags.entry(key)
                .and_modify(|(_, existing_color, count)| {
                    *count += 1;
                    if existing_color.is_none() {
                        *existing_color = color.clone();
                    }
                })
                .or_insert((tag.name.clone(), color, 1));
        }
        papers.push(TagIndexPaperView {
            path: paper.path.clone(),
            tags: paper.tags.clone(),
        });
    }
    TagIndexView {
        generated_at: file.generated_at.clone(),
        tags: tags
            .into_values()
            .map(|(name, color, count)| TagIndexEntry { name, color, count })
            .collect(),
        papers,
        untagged,
    }
}

/// Cheap read for `tags_index_read`: lazily rebuild then project.
pub fn read_tag_index(vault_root: &Path) -> Result<TagIndexView, AppError> {
    let file = ensure_tag_index(vault_root)?;
    Ok(tag_index_view(&file))
}

/// Best-effort eager refresh for hot write paths (tag/meta edits).
///
/// Failures only warn: the index is a cache and the next read rebuilds it, so a
/// tag write must never fail because the cache could not be refreshed.
pub fn ensure_tag_index_best_effort(vault_root: &Path) {
    if let Err(error) = ensure_tag_index(vault_root) {
        log::warn!(
            target: "agentero::catalog",
            "tag index refresh failed for {}: {error}",
            vault_root.display()
        );
    }
}

/// Remove the hidden cache (used when the catalog is rebuilt from scratch).
pub fn invalidate_tag_index(vault_root: &Path) {
    let path = tag_index_path(vault_root);
    if let Err(error) = std::fs::remove_file(&path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!(
                target: "agentero::catalog",
                "tag index invalidate failed for {}: {error}",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::paper::catalog::papers::{PaperRecord, PaperTag};

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("temp vault");
        std::fs::create_dir_all(root.path().join("papers/tag1/alpha")).unwrap();
        std::fs::create_dir_all(root.path().join("papers/beta")).unwrap();
        let mut alpha = PaperRecord::local_pdf("alpha".into(), "Alpha Paper".into())
            .at_path("papers/tag1/alpha");
        alpha.tags = vec![
            PaperTag {
                name: "tag1".into(),
                color: Some("blue".into()),
            },
            PaperTag {
                name: "@arxiv:cs.CL".into(),
                color: None,
            },
            PaperTag {
                name: "#submitted:NeurIPS 2017".into(),
                color: None,
            },
        ];
        papers::upsert_paper(root.path(), &alpha).unwrap();
        let beta = PaperRecord::local_pdf("beta".into(), "Beta Paper".into())
            .at_path("papers/beta");
        papers::upsert_paper(root.path(), &beta).unwrap();
        root
    }

    #[test]
    fn rebuilds_filters_internal_tags_and_lists_untagged() {
        let root = fixture();
        let vault = root.path();
        let view = read_tag_index(vault).unwrap();
        assert!(tag_index_path(vault).is_file());
        assert_eq!(
            view.tags.len(),
            1,
            "internal @arxiv and #-managed tags must be filtered"
        );
        assert_eq!(view.tags[0].name, "tag1");
        assert_eq!(view.tags[0].color.as_deref(), Some("blue"));
        assert_eq!(view.tags[0].count, 1);
        assert_eq!(view.papers.len(), 2);
        assert_eq!(view.untagged, vec!["papers/beta".to_string()]);

        // Cached read: same generatedAt (no needless rebuild).
        let again = read_tag_index(vault).unwrap();
        assert_eq!(again.generated_at, view.generated_at);

        // A tag write changes the catalog fingerprint → next read rebuilds.
        papers::set_tags(
            vault,
            "papers/beta",
            &[PaperTag::new("tag2"), PaperTag::new("tag1")],
        )
        .unwrap();
        let refreshed = read_tag_index(vault).unwrap();
        assert_eq!(refreshed.tags.len(), 2, "tag2 appears, tag1 now has 2 papers");
        let tag1 = refreshed
            .tags
            .iter()
            .find(|tag| tag.name == "tag1")
            .expect("tag1");
        assert_eq!(tag1.count, 2);
        assert!(refreshed.untagged.is_empty());
    }

    #[test]
    fn empty_vault_returns_empty_arrays() {
        let root = tempfile::tempdir().unwrap();
        let view = read_tag_index(root.path()).unwrap();
        assert!(view.tags.is_empty());
        assert!(view.papers.is_empty());
        assert!(view.untagged.is_empty());
    }

    #[test]
    fn schema_version_mismatch_forces_rebuild() {
        let root = fixture();
        let vault = root.path();
        let mut file = ensure_tag_index(vault).unwrap();
        file.version = TAG_INDEX_VERSION + 1;
        write_tag_index(vault, &file).unwrap();
        let view = read_tag_index(vault).unwrap();
        assert_eq!(
            read_cached_file(vault).map(|file| file.version),
            Some(TAG_INDEX_VERSION),
            "stale-schema file must be overwritten with a fresh index"
        );
        assert_eq!(view.tags.len(), 1);
    }

    #[test]
    fn invalidate_removes_the_cache() {
        let root = fixture();
        let vault = root.path();
        ensure_tag_index(vault).unwrap();
        assert!(tag_index_path(vault).is_file());
        invalidate_tag_index(vault);
        assert!(!tag_index_path(vault).is_file());
    }

    #[test]
    fn internal_tag_filter_ignores_legacy_arxiv_categories() {
        // `papers::is_internal_tag_name` also hides bare legacy arXiv subjects.
        assert!(papers::is_internal_tag_name("@zotero:ABCD"));
        assert!(papers::is_internal_tag_name("@arxiv:2401.01234"));
        assert!(papers::is_internal_tag_name(
            "Computer Science - Machine Learning"
        ));
        assert!(!papers::is_internal_tag_name("tag1"));
    }
}
