//! Content-domain search for the Library search box.
//!
//! The Library filters its in-memory catalog rows by metadata instantly; when
//! the user opts into extra search domains, the Web host also matches the
//! query against per-paper user content that is not in the catalog:
//!
//! - [`ContentDomain::Notes`] — `{paper}/NOTES.md`;
//! - [`ContentDomain::Marks`] — `{paper}/marks/annotations.json`
//!   highlights/comments (quotes) plus the per-id `marks/*.json` visual/ask/
//!   translate mark text;
//! - [`ContentDomain::Captions`] — figure/table/algorithm titles from
//!   `{paper}/source/layout-index.json`.
//!
//! Only paper folders present in the catalog are read (the Library table is a
//! catalog view), and the walk is backed by a process-wide cache keyed by
//! `(vault, paper)`: extracted text is reused until the source sidecars change
//! (mtime + size). A warm query only pays a few `stat()` calls per paper, so a
//! 495-paper vault stays interactive; the first query per paper pays one read
//! per enabled domain. Only matching paper paths leave the host — never the
//! source text itself.
//!
//! Exposed over the Web Host as `paper_search_content`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;
use crate::features::paper::catalog::papers;

const NOTES_FILE: &str = "NOTES.md";
const ANNOTATIONS_FILE: &str = "annotations.json";
const LAYOUT_INDEX_FILE: &str = "layout-index.json";
/// Guard against pathological sidecars (the cache holds extracted text).
const MAX_SIDECAR_BYTES: u64 = 4 * 1024 * 1024;

/// Extra content domains the Library search can match against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentDomain {
    /// `{paper}/NOTES.md`.
    Notes,
    /// Highlight quotes / comments and visual mark text under `marks/`.
    Marks,
    /// Figure/table captions from `source/layout-index.json`.
    Captions,
}

impl ContentDomain {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "notes" => Some(Self::Notes),
            "marks" => Some(Self::Marks),
            "captions" => Some(Self::Captions),
            _ => None,
        }
    }
}

/// Parse the wire `domains` array, rejecting unknown values so a typo cannot
/// silently return metadata-only results. Duplicates collapse.
pub fn parse_domains(raw: &[String]) -> Result<Vec<ContentDomain>, AppError> {
    let mut domains = Vec::with_capacity(raw.len());
    for item in raw {
        let domain = ContentDomain::parse(item).ok_or_else(|| {
            AppError::domain(
                "usage",
                format!(
                    "unknown search domain '{}' (use notes | marks | captions)",
                    item.trim()
                ),
            )
        })?;
        if !domains.contains(&domain) {
            domains.push(domain);
        }
    }
    Ok(domains)
}

/// `paper_search_content({vaultPath, query, domains})` arguments.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperContentSearchArgs {
    pub vault_path: String,
    pub query: String,
    /// `notes` | `marks` | `captions`; empty means "metadata only" (no call).
    #[serde(default)]
    pub domains: Vec<String>,
}

/// Matching catalog paper folders (vault-relative), in catalog order.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperContentSearchResult {
    pub paper_paths: Vec<String>,
}

/// Split a query into lower-cased terms; every term must match (AND), mirroring
/// `vault_search` so both search surfaces behave the same.
fn query_terms(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Resolve and search; the Web handler's entry point.
pub fn search_paper_content(
    args: &PaperContentSearchArgs,
) -> Result<PaperContentSearchResult, AppError> {
    let vault = crate::fs::resolve_vault(&args.vault_path)?;
    let domains = parse_domains(&args.domains)?;
    let paper_paths = search_content_in_vault(&vault, &args.query, &domains)?;
    Ok(PaperContentSearchResult { paper_paths })
}

/// Match `query` against the requested content domains of every catalog paper.
///
/// Returns the vault-relative paper folders whose combined enabled-domain text
/// contains every whitespace-separated query term (case-insensitive, Unicode
/// aware). An empty query / empty domain list returns no matches without
/// touching the filesystem.
pub fn search_content_in_vault(
    vault: &Path,
    query: &str,
    domains: &[ContentDomain],
) -> Result<Vec<String>, AppError> {
    let terms = query_terms(query);
    if terms.is_empty() || domains.is_empty() {
        return Ok(Vec::new());
    }

    let records = papers::list_all_unique_by_id(vault)?;
    let vault_key = crate::fs::canonicalize_best_effort(vault)
        .to_string_lossy()
        .into_owned();
    let cache = content_cache();
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut matching = Vec::new();
    for record in records {
        // Catalog paths are trusted in practice, but never let a corrupt row
        // escape the vault.
        let Ok((paper_dir, paper_rel)) = crate::fs::resolve_paper_dir(vault, &record.path) else {
            log::debug!(
                target: "agentero::paper_search_content",
                "skipping catalog row with invalid paper path: {:?}",
                record.path
            );
            continue;
        };
        let entry = cache.entry((vault_key.clone(), paper_rel)).or_default();
        let mut haystack = String::new();
        for domain in domains {
            let text = entry.text_for(*domain, &paper_dir);
            if !haystack.is_empty() && !text.is_empty() {
                haystack.push('\n');
            }
            haystack.push_str(text);
        }
        if terms.iter().all(|term| haystack.contains(term.as_str())) {
            matching.push(record.path);
        }
    }
    Ok(matching)
}

/// Fingerprint of one optional sidecar file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FileStamp {
    #[default]
    Missing,
    Present {
        mtime: Option<SystemTime>,
        size: u64,
    },
}

impl FileStamp {
    fn read(path: &Path) -> Self {
        match fs::metadata(path) {
            Ok(meta) if meta.is_file() => Self::Present {
                mtime: meta.modified().ok(),
                size: meta.len(),
            },
            _ => Self::Missing,
        }
    }
}

/// `marks/` is many files: the aggregate annotations file plus one JSON per
/// visual/ask/translate mark. The stamp covers the aggregate file and the
/// sorted per-id files so adding, removing or patching a mark invalidates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct MarksStamp {
    annotations: FileStamp,
    per_id: Vec<(String, FileStamp)>,
}

impl MarksStamp {
    fn read(paper_dir: &Path) -> Self {
        let marks_dir = paper_dir.join("marks");
        let annotations = FileStamp::read(&marks_dir.join(ANNOTATIONS_FILE));
        let mut per_id = Vec::new();
        if let Ok(entries) = fs::read_dir(&marks_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if name == ANNOTATIONS_FILE || !name.to_ascii_lowercase().ends_with(".json") {
                    continue;
                }
                per_id.push((name.to_string(), FileStamp::read(&path)));
            }
        }
        per_id.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            annotations,
            per_id,
        }
    }
}

/// Extracted, lower-cased searchable text for one paper's enabled domains.
#[derive(Debug, Default)]
struct CachedPaperText {
    notes: Option<(FileStamp, String)>,
    marks: Option<(MarksStamp, String)>,
    captions: Option<(FileStamp, String)>,
}

impl CachedPaperText {
    fn text_for(&mut self, domain: ContentDomain, paper_dir: &Path) -> &str {
        match domain {
            ContentDomain::Notes => {
                ensure_single_file_text(&mut self.notes, &paper_dir.join(NOTES_FILE), read_sidecar)
            }
            ContentDomain::Marks => {
                let stamp = MarksStamp::read(paper_dir);
                let stale = self
                    .marks
                    .as_ref()
                    .is_none_or(|(cached, _)| *cached != stamp);
                if stale {
                    let text = extract_marks_text(paper_dir).to_lowercase();
                    self.marks = Some((stamp, text));
                }
                self.marks
                    .as_ref()
                    .map(|(_, text)| text.as_str())
                    .unwrap_or_default()
            }
            ContentDomain::Captions => ensure_single_file_text(
                &mut self.captions,
                &paper_dir.join("source").join(LAYOUT_INDEX_FILE),
                |path| read_sidecar(path).map(|raw| layout_index_captions(&raw)),
            ),
        }
    }
}

type ContentCache = Mutex<HashMap<(String, String), CachedPaperText>>;

fn content_cache() -> &'static ContentCache {
    static CACHE: OnceLock<ContentCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Read a bounded UTF-8 sidecar. `None` means "missing or unreadable" —
/// callers cache that as empty text.
fn read_sidecar(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_SIDECAR_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Cache a single-file domain's extracted text; validated by (mtime, size).
fn ensure_single_file_text<'a, F>(
    slot: &'a mut Option<(FileStamp, String)>,
    path: &Path,
    extract: F,
) -> &'a str
where
    F: Fn(&Path) -> Option<String>,
{
    let stamp = FileStamp::read(path);
    let stale = slot
        .as_ref()
        .is_none_or(|(cached_stamp, _)| *cached_stamp != stamp);
    if stale {
        // A missing/malformed file caches as empty so it is not re-read on
        // every keystroke; a later create/edit changes the stamp and rebuilds.
        let text = extract(path).unwrap_or_default().to_lowercase();
        *slot = Some((stamp, text));
    }
    slot.as_ref().map(|(_, text)| text.as_str()).unwrap_or("")
}

/// `marks/annotations.json` (EmbedPDF transfer items) + per-id `marks/*.json`.
///
/// The aggregate file is the single source of truth for highlights/批注: quote
/// text lives in `annotation.custom.quote`, the comment in `annotation.contents`
/// (older rows kept both at the item root). Per-id files carry visual/ask/
/// translate marks; their user-visible text lives in `quote`/`comment`/`text`/
/// `translation`/`keyword` fields.
fn extract_marks_text(paper_dir: &Path) -> String {
    let marks_dir = paper_dir.join("marks");
    let mut parts: Vec<String> = Vec::new();

    if let Some(annotations) = read_sidecar(&marks_dir.join(ANNOTATIONS_FILE)) {
        if let Ok(value) = serde_json::from_str::<Value>(&annotations) {
            let items = match &value {
                Value::Array(items) => items.clone(),
                other => other
                    .get("annotations")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            };
            for item in &items {
                collect_mark_text(item, &mut parts);
            }
        }
    }

    if let Ok(entries) = fs::read_dir(&marks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name == ANNOTATIONS_FILE || !name.to_ascii_lowercase().ends_with(".json") {
                continue;
            }
            let Some(raw) = read_sidecar(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            collect_mark_text(&value, &mut parts);
        }
    }

    parts.join("\n")
}

/// Pull user-authored text out of one annotation / mark object.
fn collect_mark_text(value: &Value, parts: &mut Vec<String>) {
    // Transfer items nest the annotation; per-id marks keep fields at the root.
    let annotation = value.get("annotation").unwrap_or(value);
    for holder in [annotation, value] {
        for key in [
            "quote",
            "comment",
            "contents",
            "text",
            "translation",
            "keyword",
        ] {
            if let Some(text) = holder.get(key).and_then(Value::as_str) {
                let text = text.trim();
                if !text.is_empty() && !parts.iter().any(|seen| seen == text) {
                    parts.push(text.to_string());
                }
            }
        }
        for nested in ["custom", "ask", "translate"] {
            if let Some(map) = holder.get(nested) {
                for key in ["quote", "comment", "text", "translation", "keyword"] {
                    if let Some(text) = map.get(key).and_then(Value::as_str) {
                        let text = text.trim();
                        if !text.is_empty() && !parts.iter().any(|seen| seen == text) {
                            parts.push(text.to_string());
                        }
                    }
                }
            }
        }
    }
}

/// Caption/title text of the figures, tables, algorithms and formulas in
/// `source/layout-index.json` (`items[].title`; the frontend builder writes the
/// merged caption text there).
fn layout_index_captions(raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return String::new();
    };
    let Some(items) = value.get("items").and_then(Value::as_array) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for item in items {
        for key in ["title", "caption"] {
            if let Some(text) = item.get(key).and_then(Value::as_str) {
                let text = text.trim();
                if !text.is_empty() && !parts.iter().any(|seen| seen == text) {
                    parts.push(text.to_string());
                }
            }
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture {
        root: tempfile::TempDir,
        vault: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            let vault = root.path().join("vault");
            std::fs::create_dir_all(&vault).expect("vault dir");
            Self { root, vault }
        }

        fn add_paper(&self, rel: &str, title: &str, notes: Option<&str>) -> PathBuf {
            let dir = self.vault.join(rel);
            std::fs::create_dir_all(&dir).expect("paper dir");
            if let Some(notes) = notes {
                std::fs::write(dir.join(NOTES_FILE), notes).expect("notes");
            }
            let record =
                papers::PaperRecord::local_pdf(rel.replace('/', "-"), title.into()).at_path(rel);
            papers::upsert_paper(&self.vault, &record).expect("catalog row");
            dir
        }

        fn search(&self, query: &str, domains: &[ContentDomain]) -> Vec<String> {
            search_content_in_vault(&self.vault, query, domains).expect("search")
        }
    }

    fn write_annotations(paper_dir: &Path, quote: &str, comment: &str) {
        std::fs::create_dir_all(paper_dir.join("marks")).expect("marks dir");
        let payload = serde_json::json!([{
            "annotation": {
                "id": "a1",
                "pageIndex": 2,
                "contents": comment,
                "custom": { "quote": quote, "paletteKey": "yellow" }
            }
        }]);
        std::fs::write(
            paper_dir.join("marks").join(ANNOTATIONS_FILE),
            serde_json::to_string(&payload).unwrap(),
        )
        .expect("annotations");
    }

    fn write_layout_index(paper_dir: &Path, title: &str) {
        std::fs::create_dir_all(paper_dir.join("source")).expect("source dir");
        let payload = serde_json::json!({
            "schemaVersion": 1,
            "source": { "mode": "sidebar", "from": "layout.json", "generatedAt": "t", "minScore": 0.3 },
            "items": [{
                "id": "figure-1",
                "stableKey": "figure-1",
                "kind": "image",
                "section": "figure",
                "page": 3,
                "pageIndex": 2,
                "bbox": { "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4 },
                "score": 0.9,
                "title": title,
                "layoutRegionId": "region-1"
            }]
        });
        std::fs::write(
            paper_dir.join("source").join(LAYOUT_INDEX_FILE),
            serde_json::to_string(&payload).unwrap(),
        )
        .expect("layout index");
    }

    #[test]
    fn notes_match_case_insensitively_and_by_chinese_substring() {
        let fixture = Fixture::new();
        fixture.add_paper(
            "papers/quantum",
            "Quantum Paper",
            Some("# Notes\n\nQuantum entanglement with 超导 qubits."),
        );
        fixture.add_paper("papers/other", "Other Paper", Some("unrelated"));

        assert_eq!(
            fixture.search("QUANTUM", &[ContentDomain::Notes]),
            vec!["papers/quantum".to_string()]
        );
        assert_eq!(
            fixture.search("量子纠缠", &[ContentDomain::Notes]),
            Vec::<String>::new(),
            "notes contain 超导, not 量子纠缠"
        );
        assert_eq!(
            fixture.search("超导", &[ContentDomain::Notes]),
            vec!["papers/quantum".to_string()]
        );
    }

    #[test]
    fn notes_are_only_matched_when_notes_domain_is_enabled() {
        let fixture = Fixture::new();
        fixture.add_paper("papers/a", "Alpha", Some("blue apples"));

        assert!(fixture.search("blue", &[ContentDomain::Notes]).len() == 1);
        assert!(fixture.search("blue", &[ContentDomain::Marks]).is_empty());
        assert!(fixture
            .search("blue", &[ContentDomain::Captions])
            .is_empty());
        assert!(fixture.search("blue", &[]).is_empty());
    }

    #[test]
    fn all_query_terms_must_match() {
        let fixture = Fixture::new();
        fixture.add_paper("papers/a", "Alpha", Some("alpha beta gamma"));
        assert_eq!(
            fixture.search("alpha gamma", &[ContentDomain::Notes]),
            vec!["papers/a".to_string()]
        );
        assert!(fixture
            .search("alpha delta", &[ContentDomain::Notes])
            .is_empty());
        assert!(fixture.search("   ", &[ContentDomain::Notes]).is_empty());
    }

    #[test]
    fn marks_match_quote_and_comment() {
        let fixture = Fixture::new();
        let dir = fixture.add_paper("papers/marks", "Marks", Some("metadata only"));
        write_annotations(&dir, "highlighted passage", "my important note");

        assert_eq!(
            fixture.search("highlighted", &[ContentDomain::Marks]),
            vec!["papers/marks".to_string()]
        );
        assert_eq!(
            fixture.search("important note", &[ContentDomain::Marks]),
            vec!["papers/marks".to_string()]
        );
        assert!(fixture
            .search("highlighted", &[ContentDomain::Notes])
            .is_empty());
    }

    #[test]
    fn marks_match_per_id_visual_annotations() {
        let fixture = Fixture::new();
        let dir = fixture.add_paper("papers/visual", "Visual", None);
        std::fs::create_dir_all(dir.join("marks")).unwrap();
        std::fs::write(
            dir.join("marks/visual-1.json"),
            r#"{"version":1,"kind":"visual","id":"visual-1","page":2,"comment":"diagram insight"}"#,
        )
        .unwrap();

        assert_eq!(
            fixture.search("diagram insight", &[ContentDomain::Marks]),
            vec!["papers/visual".to_string()]
        );
    }

    #[test]
    fn captions_match_layout_index_titles() {
        let fixture = Fixture::new();
        let dir = fixture.add_paper("papers/captions", "Captions", Some("no caption body"));
        write_layout_index(&dir, "Fig. 2: Neural scaling law");

        assert_eq!(
            fixture.search("scaling law", &[ContentDomain::Captions]),
            vec!["papers/captions".to_string()]
        );
        assert!(fixture
            .search("scaling law", &[ContentDomain::Notes])
            .is_empty());
    }

    #[test]
    fn missing_sidecars_are_tolerated() {
        let fixture = Fixture::new();
        fixture.add_paper("papers/empty", "Empty", None);

        assert!(fixture
            .search(
                "anything",
                &[
                    ContentDomain::Notes,
                    ContentDomain::Marks,
                    ContentDomain::Captions
                ]
            )
            .is_empty());
    }

    #[test]
    fn malformed_sidecars_are_tolerated() {
        let fixture = Fixture::new();
        let dir = fixture.add_paper("papers/bad", "Bad", Some("plain"));
        std::fs::create_dir_all(dir.join("marks")).unwrap();
        std::fs::write(dir.join("marks").join(ANNOTATIONS_FILE), "{not json").unwrap();
        std::fs::create_dir_all(dir.join("source")).unwrap();
        std::fs::write(dir.join("source").join(LAYOUT_INDEX_FILE), "[]").unwrap();

        assert!(fixture
            .search(
                "plain",
                &[
                    ContentDomain::Notes,
                    ContentDomain::Marks,
                    ContentDomain::Captions
                ]
            )
            .iter()
            .any(|path| path == "papers/bad"));
    }

    #[test]
    fn only_catalog_papers_are_searched() {
        let fixture = Fixture::new();
        // On disk but not in the catalog: the Library never shows it.
        let orphan = fixture.vault.join("papers/orphan");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join(NOTES_FILE), "orphan token").unwrap();

        assert!(fixture.search("orphan", &[ContentDomain::Notes]).is_empty());
    }

    #[test]
    fn catalog_path_escaping_the_vault_is_skipped() {
        let fixture = Fixture::new();
        let outside = fixture.root.path().join("escape");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join(NOTES_FILE), "secret token").unwrap();
        // Bypass upsert validation: simulate a corrupt catalog row.
        let conn =
            crate::features::paper::catalog::ensure_catalog(&fixture.vault).expect("catalog");
        conn.execute(
            "INSERT INTO papers (path, id, type, title, added_at, updated_at) \
             VALUES ('../escape', 'escape', 'article', 'Escape', 't', 't')",
            [],
        )
        .expect("insert row");
        drop(conn);

        assert!(fixture.search("secret", &[ContentDomain::Notes]).is_empty());
    }

    #[test]
    fn cache_invalidates_when_notes_or_marks_change() {
        let fixture = Fixture::new();
        let dir = fixture.add_paper("papers/a", "Alpha", Some("first version"));
        write_annotations(&dir, "old quote", "old comment");

        assert_eq!(
            fixture.search("first", &[ContentDomain::Notes]),
            vec!["papers/a".to_string()]
        );
        assert_eq!(
            fixture.search("old quote", &[ContentDomain::Marks]),
            vec!["papers/a".to_string()]
        );

        // Different size guarantees the (mtime, size) stamp differs even on
        // coarse-mtime filesystems.
        std::fs::write(dir.join(NOTES_FILE), "second version with more text").unwrap();
        write_annotations(&dir, "fresh quote", "fresh comment");

        assert!(fixture.search("first", &[ContentDomain::Notes]).is_empty());
        assert_eq!(
            fixture.search("second version", &[ContentDomain::Notes]),
            vec!["papers/a".to_string()]
        );
        assert!(fixture
            .search("old quote", &[ContentDomain::Marks])
            .is_empty());
        assert_eq!(
            fixture.search("fresh comment", &[ContentDomain::Marks]),
            vec!["papers/a".to_string()]
        );
    }

    #[test]
    fn cache_invalidates_when_a_new_mark_file_appears() {
        let fixture = Fixture::new();
        let dir = fixture.add_paper("papers/a", "Alpha", None);
        assert!(fixture
            .search("new idea", &[ContentDomain::Marks])
            .is_empty());

        std::fs::create_dir_all(dir.join("marks")).unwrap();
        std::fs::write(
            dir.join("marks/ask-1.json"),
            r#"{"kind":"ask","id":"ask-1","comment":"new idea"}"#,
        )
        .unwrap();

        assert_eq!(
            fixture.search("new idea", &[ContentDomain::Marks]),
            vec!["papers/a".to_string()]
        );
    }

    #[test]
    fn parses_wire_domains_and_rejects_unknown() {
        let parsed = parse_domains(&[
            "notes".into(),
            " NOTES ".into(),
            "marks".into(),
            "captions".into(),
        ])
        .expect("domains");
        assert_eq!(
            parsed,
            vec![
                ContentDomain::Notes,
                ContentDomain::Marks,
                ContentDomain::Captions
            ]
        );
        let error = parse_domains(&["body".into()]).expect_err("unknown domain");
        assert_eq!(error.code(), "usage");
        assert!(error.to_string().contains("body"));
    }

    #[test]
    fn search_args_deserialize_from_camel_case_wire_shape() {
        let args: PaperContentSearchArgs = serde_json::from_value(serde_json::json!({
            "vaultPath": "/vault",
            "query": "quantum",
            "domains": ["notes", "marks"]
        }))
        .expect("args");
        assert_eq!(args.vault_path, "/vault");
        assert_eq!(args.query, "quantum");
        assert_eq!(args.domains, vec!["notes", "marks"]);
    }
}
