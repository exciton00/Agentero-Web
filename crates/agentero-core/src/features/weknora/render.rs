//! Paper package → merged Markdown for WeKnora (renderer v1).
//!
//! The document is the single RAG unit per paper: metadata header + abstract +
//! `PAPER.md` body + figure/table index + user notes + annotations/marks +
//! notes-image placeholders + a BibTeX/BibLaTeX citation section.
//!
//! Binary images never leave the Vault: every image reference is replaced with
//! a text placeholder that keeps the page/alt/caption searchable. Layout and
//! section shape are part of [`RENDERER_VERSION`]; bump it to force a re-sync.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::error::AppError;
use crate::features::paper::catalog::bibtex_export::BibEntry;
use crate::features::paper::catalog::papers::{self, PaperRecord};
use crate::features::system::settings::BibtexMountSettings;
use crate::features::pdf::layout_index;
use crate::features::weknora::{
    settings::WeKnoraSyncSettings, MAX_DOCUMENT_BYTES, RENDERER_VERSION,
};

/// Which sections the user opted into syncing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub include_paper_body: bool,
    pub include_notes: bool,
    pub include_marks: bool,
    pub include_figure_index: bool,
    pub include_citation: bool,
    pub max_bytes: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            include_paper_body: true,
            include_notes: true,
            include_marks: true,
            include_figure_index: true,
            include_citation: true,
            max_bytes: MAX_DOCUMENT_BYTES,
        }
    }
}

impl RenderOptions {
    pub fn from_settings(settings: &WeKnoraSyncSettings) -> Self {
        Self {
            include_paper_body: settings.include_paper_body,
            include_notes: settings.include_notes,
            include_marks: settings.include_marks,
            include_figure_index: settings.include_figure_index,
            include_citation: settings.include_citation,
            max_bytes: MAX_DOCUMENT_BYTES,
        }
    }

    /// Stable flag string folded into the content hash (so toggling a section
    /// invalidates the cached render even when the raw files did not change).
    pub fn hash_key(&self) -> String {
        format!(
            "body={} notes={} marks={} figures={} citation={}",
            self.include_paper_body,
            self.include_notes,
            self.include_marks,
            self.include_figure_index,
            self.include_citation
        )
    }
}

/// What actually made it into the document (for `custom_metadata` flags).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderIncludes {
    pub paper_body: bool,
    pub notes: bool,
    pub marks: bool,
    pub figure_index: bool,
    pub citation: bool,
}

/// One rendered document.
#[derive(Debug, Clone)]
pub struct RenderOutput {
    pub markdown: String,
    pub includes: RenderIncludes,
    /// True when the document hit the size cap and was truncated.
    pub truncated: bool,
    /// Citation key used in the document (empty when no mount entry).
    pub bibtex_key: String,
}

/// `sha256(renderer version + section flags + markdown)`.
///
/// `is_read` / `updated_at`-style catalog fields are intentionally absent: a
/// paper-reader run that only flips `is_read` must not re-sync the document.
pub fn content_hash(options: &RenderOptions, markdown: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(RENDERER_VERSION.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(options.hash_key().as_bytes());
    hasher.update(b"|");
    hasher.update(markdown.as_bytes());
    hex::encode(hasher.finalize())
}

/// Render one paper package into the merged Markdown document.
///
/// `bib` is the mount-scoped citation entry for this paper (`None` when the
/// mount is disabled or excludes the paper) — the caller builds the whole mount
/// index once per sync run so keys stay identical to `/agentero_online.bib`.
pub fn render_paper(
    vault: &Path,
    record: &PaperRecord,
    bib: Option<&BibEntry>,
    mount: &BibtexMountSettings,
    options: RenderOptions,
) -> Result<RenderOutput, AppError> {
    let paper_dir = vault.join(&record.path);
    let mut includes = RenderIncludes::default();
    let mut sections: Vec<String> = Vec::new();

    let title = record.title.trim();
    let title = if title.is_empty() {
        record.id.as_str()
    } else {
        title
    };
    let tags = user_tags(record);
    let key = bib.map(|entry| entry.key.clone()).unwrap_or_default();

    let mut header = String::new();
    header.push_str(&format!("# {title}\n\n"));
    header.push_str(&format!("- Agentero paper id: {}\n", record.id));
    header.push_str(&format!("- Vault path: {}\n", record.path));
    let authors = record
        .authors
        .iter()
        .map(|author| author.trim())
        .filter(|author| !author.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if !authors.is_empty() {
        header.push_str(&format!("- Authors: {authors}\n"));
    }
    let mut year_venue_doi = Vec::new();
    if let Some(year) = record.year {
        year_venue_doi.push(format!("Year: {year}"));
    } else if let Some(date) = record.date.as_deref().filter(|date| !date.trim().is_empty()) {
        year_venue_doi.push(format!("Date: {date}"));
    }
    if let Some(venue) = record
        .publication
        .as_deref()
        .map(str::trim)
        .filter(|venue| !venue.is_empty())
    {
        year_venue_doi.push(format!("Venue: {venue}"));
    }
    if let Some(doi) = record.doi.as_deref().map(str::trim).filter(|doi| !doi.is_empty()) {
        year_venue_doi.push(format!("DOI: {doi}"));
    }
    if !year_venue_doi.is_empty() {
        header.push_str(&format!("- {}\n", year_venue_doi.join(" | ")));
    }
    if !tags.is_empty() {
        header.push_str(&format!("- Tags: {}\n", tags.join(", ")));
    }
    if !key.is_empty() {
        header.push_str(&format!("- BibTeX key: `{key}`\n"));
        header.push_str(&format!("- LaTeX: `\\cite{{{key}}}`\n"));
    }
    sections.push(header.trim_end().to_string());

    if let Some(abstract_text) = record
        .abstract_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        sections.push(format!("## Abstract\n{abstract_text}"));
    }

    let paper_body = if options.include_paper_body {
        read_optional(paper_dir.join("PAPER.md"))?
    } else {
        None
    };
    if let Some(body) = &paper_body {
        let (body, _) = replace_markdown_images(body, |alt, url| {
            figure_placeholder(alt, url)
        });
        let body = body.trim();
        if !body.is_empty() {
            sections.push(format!("## Paper body\n{body}"));
            includes.paper_body = true;
        }
    }

    if options.include_figure_index {
        let index = figure_index(vault, record, paper_body.as_deref());
        if !index.is_empty() {
            sections.push(format!(
                "## Figure and table index\n{}",
                index.join("\n")
            ));
            includes.figure_index = true;
        }
    }

    let notes = if options.include_notes {
        read_optional(paper_dir.join("NOTES.md"))?
    } else {
        None
    };
    let mut note_images: Vec<String> = Vec::new();
    if let Some(notes) = &notes {
        let (frontmatter, body) = crate::features::markdown::frontmatter::split_off_frontmatter(notes);
        let mut notes_section = String::new();
        let fields = parse_simple_frontmatter(&frontmatter);
        if let Some(aliases) = fields.get("aliases").filter(|values| !values.is_empty()) {
            notes_section.push_str(&format!("- aliases: {}\n", aliases.join("; ")));
        }
        if let Some(created) = fields.get("created").filter(|values| !values.is_empty()) {
            notes_section.push_str(&format!("- created: {}\n", created.join("; ")));
        }
        if let Some(tags) = fields.get("tags").filter(|values| !values.is_empty()) {
            notes_section.push_str(&format!("- tags: {}\n", tags.join(", ")));
        }
        let (body, images) = replace_markdown_images(&body, |alt, url| {
            note_image_placeholder(alt, url)
        });
        note_images = images
            .into_iter()
            .map(|url| normalize_note_image_path(&url))
            .collect();
        let body = body.trim();
        if !body.is_empty() {
            if !notes_section.is_empty() {
                notes_section.push('\n');
            }
            notes_section.push_str(body);
            sections.push(format!("## User notes\n{}", notes_section.trim_end()));
            includes.notes = true;
        }
    }

    if options.include_marks {
        let marks = marks_section(&paper_dir)?;
        if !marks.is_empty() {
            sections.push(format!("## Annotations and marks\n{}", marks.join("\n")));
            includes.marks = true;
        }
    }

    if !note_images.is_empty() {
        let mut lines = Vec::new();
        let mut seen = HashSet::new();
        for image in note_images {
            if seen.insert(image.clone()) {
                lines.push(format!("- {image}"));
            }
        }
        sections.push(format!(
            "## Notes images (binary not uploaded)\n{}",
            lines.join("\n")
        ));
    }

    if options.include_citation {
        let citation = citation_section(bib, mount);
        sections.push(citation);
        includes.citation = true;
    }

    let mut markdown = sections.join("\n\n");
    markdown.push('\n');
    let mut truncated = false;
    if markdown.len() > options.max_bytes {
        markdown = truncate_at_char_boundary(&markdown, options.max_bytes);
        markdown.push_str("\n\n<!-- truncated: document exceeded the size cap -->\n");
        truncated = true;
    }
    Ok(RenderOutput {
        markdown,
        includes,
        truncated,
        bibtex_key: key,
    })
}

/// Catalog tags minus internal provenance tags (`@zotero:…`, `#…`).
pub fn user_tags(record: &PaperRecord) -> Vec<String> {
    let mut tags = Vec::new();
    for tag in &record.tags {
        let name = tag.name.trim();
        if !name.is_empty() && !papers::is_internal_tag_name(name) && !tags.iter().any(|t| t == name)
        {
            tags.push(name.to_string());
        }
    }
    tags
}

fn read_optional(path: std::path::PathBuf) -> Result<Option<String>, AppError> {
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::message(format!(
            "read {}: {error}",
            path.display()
        ))),
    }
}

fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

// ---------------------------------------------------------------------------
// image placeholders
// ---------------------------------------------------------------------------

/// Replace every `![alt](url)` with `placeholder(alt, url)`; returns the new
/// text plus the URLs encountered (in order, for the notes-image list).
fn replace_markdown_images<F>(text: &str, placeholder: F) -> (String, Vec<String>)
where
    F: Fn(&str, &str) -> String,
{
    let mut out = String::with_capacity(text.len());
    let mut images = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        out.push_str(&rest[..start]);
        let after_bang = &rest[start + 2..];
        let Some(alt_end) = after_bang.find(']') else {
            out.push_str(&rest[start..]);
            return (out, images);
        };
        let alt = &after_bang[..alt_end];
        let after_alt = &after_bang[alt_end + 1..];
        if !after_alt.starts_with('(') {
            out.push_str(&rest[start..start + 2 + alt_end + 1]);
            rest = &rest[start + 2 + alt_end + 1..];
            continue;
        }
        let after_paren = &after_alt[1..];
        let Some(url_end) = after_paren.find(')') else {
            out.push_str(&rest[start..]);
            return (out, images);
        };
        let url = after_paren[..url_end].trim();
        images.push(url.to_string());
        out.push_str(&placeholder(alt, url));
        rest = &after_paren[url_end + 1..];
    }
    out.push_str(rest);
    (out, images)
}

fn figure_placeholder(_alt: &str, url: &str) -> String {
    match page_from_image_url(url) {
        Some(page) => format!("[figure on page {page}]"),
        None => "[figure]".to_string(),
    }
}

fn note_image_placeholder(alt: &str, url: &str) -> String {
    let name = url
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(url);
    let alt = alt.trim();
    if alt.is_empty() {
        format!("[笔记图片 ({name})]")
    } else {
        format!("[笔记图片 {alt} ({name})]")
    }
}

/// Vault-relative image path used in the `Notes images` list.
fn normalize_note_image_path(url: &str) -> String {
    let url = url.trim().replace('\\', "/");
    url.trim_start_matches("./").to_string()
}

fn page_from_image_url(url: &str) -> Option<u32> {
    let marker = url.find("page_").or_else(|| url.find("page-"))?;
    let digits = &url[marker + 5..];
    let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u32>().ok().filter(|page| *page > 0)
}

// ---------------------------------------------------------------------------
// figure / table index
// ---------------------------------------------------------------------------

fn figure_index(vault: &Path, record: &PaperRecord, paper_body: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    if let Ok(listed) = layout_index::list_regions(vault, &record.path, &[], None) {
        for item in listed.items {
            let title = item
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or(item.stable_key.trim())
                .to_string();
            if title.is_empty() {
                continue;
            }
            let key = (item.page, title.to_ascii_lowercase());
            if !seen.insert(key) {
                continue;
            }
            lines.push(format!(
                "- [page {}, {}] {title}",
                item.page,
                item.kind.trim()
            ));
        }
    }
    if lines.is_empty() {
        if let Some(body) = paper_body {
            lines = fallback_figure_lines(body);
        }
    }
    lines
}

/// PAPER.md fallback when no `layout-index.json` exists: keep `Fig.` / `Table`
/// caption lines as text (page unknown at this point).
fn fallback_figure_lines(body: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.len() < 6 {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        let kind = if lower.starts_with("fig.") || lower.starts_with("figure ") {
            "figure"
        } else if lower.starts_with("table ") || lower.starts_with("table.") {
            "table"
        } else {
            continue;
        };
        if !seen.insert(lower.clone()) {
            continue;
        }
        lines.push(format!("- [{kind}] {line}"));
        if lines.len() >= 200 {
            break;
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// notes frontmatter (subset: aliases/created/tags)
// ---------------------------------------------------------------------------

fn parse_simple_frontmatter(frontmatter: &str) -> BTreeMap<String, Vec<String>> {
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for raw in frontmatter.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(item) = trimmed.strip_prefix("- ") {
            if let Some(key) = &current {
                let value = item.trim().trim_matches(['"', '\'']).trim();
                if !value.is_empty() {
                    fields
                        .entry(key.clone())
                        .or_default()
                        .push(value.to_string());
                }
            }
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().trim_matches(['"', '\'']).trim();
            if value.is_empty() {
                current = Some(key);
            } else {
                fields
                    .entry(key)
                    .or_default()
                    .push(value.to_string());
                current = None;
            }
        }
    }
    fields
}

// ---------------------------------------------------------------------------
// annotations / marks
// ---------------------------------------------------------------------------

fn marks_section(paper_dir: &Path) -> Result<Vec<String>, AppError> {
    let mut lines = Vec::new();
    let marks_dir = paper_dir.join("marks");
    if !marks_dir.is_dir() {
        return Ok(lines);
    }
    let annotations_path = marks_dir.join("annotations.json");
    if let Some(text) = read_optional(annotations_path.clone())? {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            let items = if let Some(array) = value.as_array() {
                array.clone()
            } else {
                value
                    .get("annotations")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            };
            for item in items {
                if let Some(line) = annotation_line(&item) {
                    lines.push(line);
                }
            }
        }
    }
    let mut mark_files: Vec<_> = std::fs::read_dir(&marks_dir)
        .map_err(|error| AppError::message(format!("read marks dir: {error}")))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
                && path.file_name().is_some_and(|name| name != "annotations.json")
        })
        .collect();
    mark_files.sort();
    for path in mark_files {
        let Some(text) = read_optional(path.clone())? else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(line) = visual_mark_line(&value) {
            lines.push(line);
        }
    }
    // Stable order: by page then by original text (annotations have no ids we
    // want to expose); keeps hashes and snapshots deterministic.
    lines.sort();
    Ok(lines)
}

fn annotation_line(item: &Value) -> Option<String> {
    let annotation = item.get("annotation").unwrap_or(item);
    let page = annotation
        .get("pageIndex")
        .and_then(Value::as_u64)
        .map(|index| index as u32 + 1)
        .or_else(|| item.get("page").and_then(Value::as_u64).map(|page| page as u32));
    let quote = annotation
        .get("custom")
        .and_then(|custom| custom.get("quote"))
        .and_then(Value::as_str)
        .or_else(|| item.get("quote").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let note = annotation
        .get("contents")
        .and_then(Value::as_str)
        .or_else(|| item.get("contents").and_then(Value::as_str))
        .or_else(|| item.get("comment").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if quote.is_none() && note.is_none() {
        return None;
    }
    let mut line = String::from("- page ");
    line.push_str(&page.map(|page| page.to_string()).unwrap_or_else(|| "?".into()));
    line.push_str(" | annotation");
    if let Some(quote) = quote {
        line.push_str(&format!(" | quote: {quote}"));
    }
    if let Some(note) = note {
        line.push_str(&format!(" | note: {note}"));
    }
    Some(line)
}

fn visual_mark_line(value: &Value) -> Option<String> {
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .unwrap_or("visual");
    let page = value
        .get("page")
        .and_then(Value::as_u64)
        .map(|page| page.to_string())
        .unwrap_or_else(|| "?".into());
    let note = ["comment", "text", "translation", "keyword"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let image = value
        .get("image")
        .and_then(|image| image.get("path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty());
    if note.is_none() && image.is_none() {
        return None;
    }
    let mut line = format!("- page {page} | {kind}");
    if let Some(note) = note {
        line.push_str(&format!(" | note: {note}"));
    }
    if let Some(image) = image {
        line.push_str(&format!(" | image: {image}"));
    }
    Some(line)
}

// ---------------------------------------------------------------------------
// citation
// ---------------------------------------------------------------------------

fn citation_section(bib: Option<&BibEntry>, mount: &BibtexMountSettings) -> String {
    let dialect = if mount
        .format
        .trim()
        .eq_ignore_ascii_case("biblatex")
    {
        "biblatex"
    } else {
        "bibtex"
    };
    let mut section = String::from("## Citation (BibTeX / BibLaTeX)\n\n");
    match bib {
        Some(entry) => {
            section.push_str(&format!("- BibTeX key: `{}`\n", entry.key));
            section.push_str(&format!("- 引用命令: `\\cite{{{}}}`\n", entry.key));
            section.push_str(&format!(
                "- 挂载地址: /agentero_online.bib（当前 dialect: {dialect}；key 两种 dialect 相同）\n"
            ));
            section.push_str(&mount_scope_line(mount));
            if !mount.enabled {
                section.push_str(
                    "- 警告: 当前 /agentero_online.bib 未启用（检查 Settings → Experimental 的挂载开关）\n",
                );
            }
            section.push_str("\n### BibTeX entry\n```bibtex\n");
            section.push_str(entry.bibtex.trim());
            section.push_str("\n```\n\n### BibLaTeX entry\n```bibtex\n");
            section.push_str(entry.biblatex.trim());
            section.push_str("\n```");
        }
        None => {
            section.push_str(&mount_scope_line(mount));
            if mount.enabled {
                section.push_str(
                    "- 警告: 当前 /agentero_online.bib 未包含本篇（检查 Settings → Experimental 的 mount tags/dirs）\n",
                );
            } else {
                section.push_str(
                    "- 警告: 当前 /agentero_online.bib 未启用（检查 Settings → Experimental 的挂载开关）\n",
                );
            }
        }
    }
    section
}

fn mount_scope_line(mount: &BibtexMountSettings) -> String {
    let tags = if mount.tags.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", mount.tags.join(", "))
    };
    let dirs = if mount.dirs.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", mount.dirs.join(", "))
    };
    let suffix = if mount.tags.is_empty() && mount.dirs.is_empty() {
        "（无过滤）"
    } else {
        ""
    };
    format!("- mount 范围: tags={tags} dirs={dirs}{suffix}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::paper::catalog::papers::{PaperKind, PaperRecord, PaperTag};
    use crate::features::system::settings::BibtexMountSettings;
    use std::fs;

    fn record(path: &str) -> PaperRecord {
        let mut record = PaperRecord::local_pdf(
            path.rsplit('/').next().unwrap_or("demo").to_string(),
            "A New Differential Amplitude Spectrum for GaN HEMTs".into(),
        )
        .at_path(path);
        record.paper_type = PaperKind::Doi;
        record.authors = vec!["Xiang Zheng".into(), "Shiwei Feng".into()];
        record.year = Some(2017);
        record.publication = Some("IEEE Transactions on Electron Devices".into());
        record.doi = Some("10.1109/TED.2017.2654481".into());
        record.abstract_text = Some("A differential amplitude spectrum is reported.".into());
        record.tags = vec![
            PaperTag::new("GaN"),
            PaperTag::new("trapping"),
            PaperTag::new("@zotero:ABCD"),
        ];
        record
    }

    fn bib() -> BibEntry {
        BibEntry {
            path: "papers/demo".into(),
            key: "zheng2017".into(),
            bibtex: "@article{zheng2017,\n  title = {Demo}\n}".into(),
            biblatex: "@article{zheng2017,\n  date = {2017}\n}".into(),
        }
    }

    fn mount(enabled: bool) -> BibtexMountSettings {
        BibtexMountSettings {
            enabled,
            format: "bibtex".into(),
            tags: Vec::new(),
            dirs: Vec::new(),
        }
    }

    fn fixture_vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let paper = dir.path().join("papers/demo");
        fs::create_dir_all(paper.join("source")).unwrap();
        fs::create_dir_all(paper.join("marks/assets")).unwrap();
        fs::create_dir_all(paper.join("assets")).unwrap();
        fs::write(
            paper.join("PAPER.md"),
            "# Paper\n\nIntro text.\n\n![](images/page_1_chart_1.jpg)\n\nFig. 1. Structure of the device.\n\n## II. METHOD\n\nMore text.\n\n![](images/page_2_formula_0.jpg)\n",
        )
        .unwrap();
        fs::write(
            paper.join("NOTES.md"),
            "---\naliases:\n  - \"Zheng 2017\"\ncreated: 2026-10-24\ntags:\n  - GaN\n---\n\n# 笔记\n\n看图 ![Figure 1: 图注](assets/fig1.jpg) 与 ![x](./assets/fig2.jpg)\n\n正文内容。\n",
        )
        .unwrap();
        fs::write(
            paper.join("marks/annotations.json"),
            r#"[{"annotation":{"pageIndex":2,"custom":{"quote":"To compare values"},"contents":"给出了公式"}}]"#,
        )
        .unwrap();
        fs::write(
            paper.join("marks/visual1.json"),
            r#"{"kind":"visual","page":4,"comment":"怎么还用excel画图","image":{"path":"assets/visual1.png"}}"#,
        )
        .unwrap();
        fs::write(
            paper.join("source/layout-index.json"),
            r#"{"schemaVersion":1,"source":{"mode":"sidebar","generatedAt":"2026-10-24T00:00:00Z","minScore":0.3},"items":[{"id":"figure-1","stableKey":"p2:figure:Fig. 1. Structure","kind":"chart","section":"figure","page":2,"pageIndex":1,"bbox":{"x":0,"y":0,"w":1,"h":1},"score":0.9,"title":"Fig. 1. Structure of the device.","layoutRegionId":"r1"},{"id":"figure-1b","stableKey":"p2:figure:Fig. 1. Structure","kind":"chart","section":"figure","page":2,"pageIndex":1,"bbox":{"x":0,"y":0,"w":1,"h":1},"score":0.8,"title":"Fig. 1. Structure of the device.","layoutRegionId":"r2"}]}"#,
        )
        .unwrap();
        fs::write(paper.join("assets/fig1.jpg"), b"binary").unwrap();
        fs::write(paper.join("assets/fig2.jpg"), b"binary").unwrap();
        dir
    }

    fn render_fixture(vault: &Path, record: &PaperRecord, options: RenderOptions) -> RenderOutput {
        render_paper(vault, record, Some(&bib()), &mount(true), options).unwrap()
    }

    #[test]
    fn renders_all_sections_with_textual_image_placeholders() {
        let vault = fixture_vault();
        let record = record("papers/demo");
        let out = render_fixture(vault.path(), &record, RenderOptions::default());
        let md = &out.markdown;

        assert!(md.starts_with("# A New Differential Amplitude Spectrum for GaN HEMTs"));
        assert!(md.contains("- Agentero paper id: demo"));
        assert!(md.contains("- Vault path: papers/demo"));
        assert!(md.contains("- Authors: Xiang Zheng, Shiwei Feng"));
        assert!(md.contains("- Year: 2017 | Venue: IEEE Transactions on Electron Devices | DOI: 10.1109/TED.2017.2654481"));
        assert!(md.contains("- Tags: GaN, trapping"), "internal tags filtered");
        assert!(!md.contains("@zotero:"), "internal tags must not leak");
        assert!(md.contains("- BibTeX key: `zheng2017`"));
        assert!(md.contains("- LaTeX: `\\cite{zheng2017}`"));
        assert!(md.contains("## Abstract\nA differential amplitude spectrum is reported."));
        assert!(md.contains("## Paper body"));
        assert!(md.contains("[figure on page 1]"));
        assert!(md.contains("[figure on page 2]"));
        assert!(!md.contains("images/page_1"), "image URLs are gone");
        assert!(!md.contains("![]("), "no binary markdown images");
        assert!(md.contains("## Figure and table index"));
        assert_eq!(
            md.matches("- [page 2, chart] Fig. 1. Structure of the device.").count(),
            1,
            "layout index dedupes identical (page,title)"
        );
        assert!(md.contains("## User notes"));
        assert!(md.contains("- aliases: Zheng 2017"), "aliases kept as text");
        assert!(md.contains("- created: 2026-10-24"));
        assert!(md.contains("- tags: GaN"));
        assert!(md.contains("[笔记图片 Figure 1: 图注 (fig1.jpg)]"));
        assert!(md.contains("[笔记图片 x (fig2.jpg)]"));
        assert!(!md.contains("assets/fig1.jpg)"), "notes image links rewritten");
        assert!(md.contains("## Annotations and marks"));
        assert!(md.contains("- page 3 | annotation | quote: To compare values | note: 给出了公式"));
        assert!(md.contains("- page 4 | visual | note: 怎么还用excel画图 | image: assets/visual1.png"));
        assert!(md.contains("## Notes images (binary not uploaded)"));
        assert!(md.contains("- assets/fig1.jpg"));
        assert!(md.contains("- assets/fig2.jpg"));
        assert!(md.contains("## Citation (BibTeX / BibLaTeX)"));
        assert!(md.contains("### BibTeX entry"));
        assert!(md.contains("@article{zheng2017"));
        assert!(md.contains("### BibLaTeX entry"));
        assert!(!out.truncated);
        assert!(out.includes.paper_body && out.includes.notes && out.includes.marks);
        assert!(out.includes.figure_index && out.includes.citation);
        assert_eq!(out.bibtex_key, "zheng2017");
    }

    #[test]
    fn hash_ignores_is_read_but_tracks_content_and_options() {
        let vault = fixture_vault();
        let mut record = record("papers/demo");
        let options = RenderOptions::default();
        let first = render_fixture(vault.path(), &record, options);
        let hash = content_hash(&options, &first.markdown);

        record.is_read = !record.is_read;
        record.updated_at = "2099-01-01T00:00:00Z".into();
        let after_read = render_fixture(vault.path(), &record, options);
        assert_eq!(
            content_hash(&options, &after_read.markdown),
            hash,
            "is_read / updated_at must not change the hash"
        );

        fs::write(
            vault.path().join("papers/demo/NOTES.md"),
            "---\naliases: [x]\n---\n\nnew note\n",
        )
        .unwrap();
        let after_edit = render_fixture(vault.path(), &record, options);
        assert_ne!(content_hash(&options, &after_edit.markdown), hash);

        let mut smaller = options;
        smaller.include_marks = false;
        let reduced = render_fixture(vault.path(), &record, smaller);
        assert_ne!(content_hash(&smaller, &reduced.markdown), hash);
        assert!(!reduced.includes.marks);
        assert!(!reduced.markdown.contains("## Annotations and marks"));
    }

    #[test]
    fn missing_paper_body_still_renders_metadata_notes_and_marks() {
        let vault = fixture_vault();
        fs::remove_file(vault.path().join("papers/demo/PAPER.md")).unwrap();
        let record = record("papers/demo");
        let out = render_fixture(vault.path(), &record, RenderOptions::default());
        assert!(!out.includes.paper_body);
        assert!(!out.markdown.contains("## Paper body"));
        assert!(out.markdown.contains("## Abstract"));
        assert!(out.markdown.contains("## User notes"));
        assert!(out.markdown.contains("## Annotations and marks"));
        // No layout index fallback either (index still present, so this section
        // comes from the index file, not from PAPER.md).
        assert!(out.markdown.contains("## Figure and table index"));
    }

    #[test]
    fn fallback_figure_parser_uses_paper_body_when_index_is_missing() {
        let vault = fixture_vault();
        fs::remove_file(vault.path().join("papers/demo/source/layout-index.json")).unwrap();
        let record = record("papers/demo");
        let out = render_fixture(vault.path(), &record, RenderOptions::default());
        assert!(out.markdown.contains("## Figure and table index"));
        assert!(
            out.markdown.contains("- [figure] Fig. 1. Structure of the device."),
            "{}",
            out.markdown
        );
    }

    #[test]
    fn paper_reader_notes_assets_stay_textual() {
        let vault = fixture_vault();
        fs::write(
            vault.path().join("papers/demo/NOTES.md"),
            "# 精读\n\n![Figure 3: DAS 变换](assets/fig3.jpg)\n",
        )
        .unwrap();
        let record = record("papers/demo");
        let out = render_fixture(vault.path(), &record, RenderOptions::default());
        assert!(out.markdown.contains("[笔记图片 Figure 3: DAS 变换 (fig3.jpg)]"));
        assert!(out.markdown.contains("- assets/fig3.jpg"));
        assert!(!out.markdown.contains("assets/fig3.jpg)"));
    }

    #[test]
    fn citation_warns_when_paper_is_outside_the_mount_scope() {
        let vault = fixture_vault();
        let record = record("papers/demo");
        let out = render_paper(
            vault.path(),
            &record,
            None,
            &mount(true),
            RenderOptions::default(),
        )
        .unwrap();
        assert!(out.markdown.contains("未包含本篇"));
        assert!(!out.markdown.contains("### BibTeX entry"));
        assert!(out.bibtex_key.is_empty());

        let disabled = render_paper(
            vault.path(),
            &record,
            Some(&bib()),
            &mount(false),
            RenderOptions::default(),
        )
        .unwrap();
        assert!(disabled.markdown.contains("未启用"));
        assert!(disabled.markdown.contains("@article{zheng2017"));
    }

    #[test]
    fn oversized_documents_are_truncated_with_a_marker() {
        let vault = fixture_vault();
        let record = record("papers/demo");
        let mut options = RenderOptions::default();
        options.max_bytes = 500;
        let out = render_fixture(vault.path(), &record, options);
        assert!(out.truncated);
        assert!(out.markdown.contains("truncated"));
        assert!(out.markdown.len() < 800);
    }

    #[test]
    fn replaces_inline_and_data_uri_images() {
        let (text, images) = replace_markdown_images(
            "before ![alt](images/page_7_chart_0.jpg) middle ![x](data:image/png;base64,AAA) after",
            |alt, url| format!("<{alt}|{url}>"),
        );
        assert_eq!(images.len(), 2);
        assert!(text.contains("<alt|images/page_7_chart_0.jpg>"));
        assert_eq!(
            figure_placeholder("", "images/page_7_chart_0.jpg"),
            "[figure on page 7]"
        );
        assert_eq!(figure_placeholder("", "data:image/png;base64,AAA"), "[figure]");
    }

    #[test]
    fn user_tags_come_from_catalog_and_drop_internal_provenance() {
        let record = record("papers/demo");
        assert_eq!(user_tags(&record), vec!["GaN", "trapping"]);
    }
}
