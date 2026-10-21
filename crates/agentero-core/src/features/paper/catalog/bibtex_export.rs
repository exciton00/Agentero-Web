//! Local BibTeX / BibLaTeX serialization for the experimental network mount.
//!
//! The Web host serves the current Vault's bibliography over `GET /bibtex` or
//! `GET /biblatex`. Serialization happens **locally** from `catalog.sqlite`:
//! no metadata is ever sent to a cloud translation service (self-hosted users
//! must not leak their library to a third party).
//!
//! Filtering (both filters empty = everything):
//! - `tags`: OR over user tags (internal provenance tags are ignored);
//! - `dirs`: vault-relative `papers/…` prefix matching with segment boundaries
//!   (`papers/tag1` never matches `papers/tag10`).
//!
//! Both filters together are an AND (tag match AND directory match).

use std::collections::HashSet;
use std::path::Path;

use serde_json::Value;

use crate::error::AppError;
use crate::features::paper::catalog::papers::{self, PaperRecord};
use crate::features::system::settings::BibtexMountSettings;
use crate::time::now_rfc3339_millis;

/// Mount flavour: which output dialect is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BibFormat {
    Bibtex,
    Biblatex,
}

impl BibFormat {
    /// Parse the settings value (`bibtex` | `biblatex`), defaulting to BibTeX.
    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("biblatex") {
            Self::Biblatex
        } else {
            Self::Bibtex
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bibtex => "bibtex",
            Self::Biblatex => "biblatex",
        }
    }

    /// Exact HTTP mount path for this dialect.
    pub fn mount_path(self) -> &'static str {
        match self {
            Self::Bibtex => "/bibtex",
            Self::Biblatex => "/biblatex",
        }
    }
}

/// One rendered response.
#[derive(Debug, Clone)]
pub struct RenderedBib {
    /// BibTeX/BibLaTeX body (UTF-8); empty when nothing matches.
    pub body: String,
    /// Number of entries in `body` (`X-Agentero-Bib-Count`).
    pub count: usize,
    /// Generation timestamp reported by `/api/bibtex/status`.
    pub generated_at: String,
}

/// Serve the mount: render every catalog paper matching the configured scope.
pub fn render(
    vault_root: &Path,
    format: BibFormat,
    mount: &BibtexMountSettings,
) -> Result<RenderedBib, AppError> {
    let records = papers::list_all(vault_root)?;
    let mut matched: Vec<PaperRecord> = records
        .into_iter()
        .filter(|record| record_matches(record, &mount.tags, &mount.dirs))
        .collect();
    matched.sort_by(|left, right| left.path.cmp(&right.path));

    let mut used_keys: HashSet<String> = HashSet::new();
    let mut entries: Vec<(String, String)> = Vec::with_capacity(matched.len());
    for record in &matched {
        let key = unique_entry_key(record, &mut used_keys);
        entries.push((key.clone(), render_entry(record, format, &key)));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let body = entries
        .into_iter()
        .map(|(_, entry)| entry)
        .collect::<Vec<_>>()
        .join("\n\n");
    let body = if body.is_empty() {
        body
    } else {
        format!("{body}\n")
    };
    Ok(RenderedBib {
        body,
        count: matched.len(),
        generated_at: now_rfc3339_millis(),
    })
}

fn normalize_path_for_match(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

/// Directory prefix match with segment boundaries: `papers/tag1` matches
/// `papers/tag1/x` but not `papers/tag10/x`.
fn path_matches_dir(path: &str, dir: &str) -> bool {
    let path = normalize_path_for_match(path);
    let dir = normalize_path_for_match(dir);
    if dir.is_empty() {
        return true;
    }
    path == dir || path.starts_with(&format!("{dir}/"))
}

/// Does one catalog record pass the mount scope? Tag OR within each filter,
/// AND across the two filters.
pub fn record_matches(record: &PaperRecord, tags: &[String], dirs: &[String]) -> bool {
    let tags_match = tags.is_empty()
        || record.tags.iter().any(|paper_tag| {
            !papers::is_internal_tag_name(&paper_tag.name)
                && tags.iter().any(|wanted| {
                    let wanted = wanted.trim();
                    !wanted.is_empty() && paper_tag.name.eq_ignore_ascii_case(wanted)
                })
        });
    if !tags_match {
        return false;
    }
    dirs.is_empty() || dirs.iter().any(|dir| path_matches_dir(&record.path, dir))
}

// ---------------------------------------------------------------------------
// entry keys
// ---------------------------------------------------------------------------

/// Primary key component: explicit `bibtex_key`, else first-author surname +
/// year. Falls back to `paper` when neither is available.
fn key_base(record: &PaperRecord) -> String {
    if let Some(key) = record
        .bibtex_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        let cleaned: String = key.chars().filter(|c| !c.is_whitespace()).collect();
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    let author_part: String = first_author_surname(record)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    if !author_part.is_empty() {
        return match year_of(record) {
            Some(year) => format!("{author_part}{year}"),
            None => author_part,
        };
    }
    // No usable author: fall back to the (folder-safe) catalog id.
    let id_part: String = record
        .id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let id_part = if id_part.is_empty() {
        "paper".to_string()
    } else {
        id_part
    };
    match year_of(record) {
        Some(year) => format!("{id_part}{year}"),
        None => id_part,
    }
}

/// Deduplicate keys deterministically with `a`, `b`, … suffixes.
fn unique_entry_key(record: &PaperRecord, used: &mut HashSet<String>) -> String {
    let base = key_base(record);
    if used.insert(base.to_ascii_lowercase()) {
        return base;
    }
    for suffix in 'a'..='z' {
        let candidate = format!("{base}{suffix}");
        if used.insert(candidate.to_ascii_lowercase()) {
            return candidate;
        }
    }
    let mut index = 2usize;
    loop {
        let candidate = format!("{base}{index}");
        if used.insert(candidate.to_ascii_lowercase()) {
            return candidate;
        }
        index += 1;
    }
}

fn first_author_surname(record: &PaperRecord) -> String {
    if let Some(creators) = record.creators.as_ref().and_then(Value::as_array) {
        for creator in creators {
            if let Some(name) = creator
                .get("lastName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                return name.to_string();
            }
            if let Some(name) = creator
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                return name.split_whitespace().next_back().unwrap_or(name).to_string();
            }
        }
    }
    let Some(author) = record
        .authors
        .first()
        .map(|author| author.trim())
        .filter(|author| !author.is_empty())
    else {
        return String::new();
    };
    if author.contains(',') {
        return author
            .split(',')
            .next()
            .map(|part| part.trim().to_string())
            .unwrap_or_default();
    }
    author
        .split_whitespace()
        .next_back()
        .map(str::to_string)
        .unwrap_or_default()
}

fn year_of(record: &PaperRecord) -> Option<i32> {
    record.year.or_else(|| {
        record
            .date
            .as_deref()
            .map(str::trim)
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse::<i32>().ok())
    })
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

fn entry_type(record: &PaperRecord, format: BibFormat) -> &'static str {
    let zotero = record
        .zotero_item_type
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '_', '-'], "");
    match zotero.as_str() {
        "journalarticle" | "article" => "article",
        "conferencepaper" => "inproceedings",
        "book" => "book",
        "booksection" => "incollection",
        "thesis" => "phdthesis",
        "report" => "techreport",
        "patent" => "patent",
        "preprint" => match format {
            BibFormat::Biblatex => "online",
            BibFormat::Bibtex => "misc",
        },
        "webpage" | "blogpost" | "forum post" | "forumpost" | "encyclopediaarticle" => {
            match format {
                BibFormat::Biblatex => "online",
                BibFormat::Bibtex => "misc",
            }
        }
        _ => {
            if record.arxiv_id.is_some() && record.publication.is_none() {
                match format {
                    BibFormat::Biblatex => "online",
                    BibFormat::Bibtex => "misc",
                }
            } else if record.publication.is_some() {
                "article"
            } else {
                "misc"
            }
        }
    }
}

/// Render one catalog record as a `@type{key, …}` block.
fn render_entry(record: &PaperRecord, format: BibFormat, key: &str) -> String {
    let entry_type = entry_type(record, format);
    let mut fields: Vec<(&str, String)> = Vec::new();

    push_field(&mut fields, "title", record.title.trim());
    let authors = author_field(record);
    if !authors.is_empty() {
        push_field(&mut fields, "author", &authors);
    }
    match format {
        BibFormat::Biblatex => {
            if let Some(date) = record
                .date
                .as_deref()
                .map(str::trim)
                .filter(|date| !date.is_empty())
            {
                push_field(&mut fields, "date", date);
            } else if let Some(year) = record.year {
                push_field(&mut fields, "year", &year.to_string());
            }
        }
        BibFormat::Bibtex => {
            if let Some(year) = year_of(record) {
                push_field(&mut fields, "year", &year.to_string());
            }
        }
    }
    let venue_field = match entry_type {
        "article" => "journal",
        "inproceedings" | "incollection" => "booktitle",
        _ => "journal",
    };
    if let Some(publication) = record
        .publication
        .as_deref()
        .map(str::trim)
        .filter(|publication| !publication.is_empty())
    {
        push_field(&mut fields, venue_field, publication);
    }
    push_optional(&mut fields, "volume", record.volume.as_deref());
    push_optional(&mut fields, "number", record.issue.as_deref());
    if let Some(pages) = record.pages.as_deref().map(normalize_pages) {
        if !pages.is_empty() {
            push_field(&mut fields, "pages", &pages);
        }
    }
    push_optional(&mut fields, "publisher", record.publisher.as_deref());
    push_optional(&mut fields, "doi", record.doi.as_deref());
    let url = record
        .html_url
        .as_deref()
        .or(record.pdf_url.as_deref())
        .or(record.source_url.as_deref());
    push_optional(&mut fields, "url", url);
    push_optional(&mut fields, "isbn", record.isbn.as_deref());
    push_optional(&mut fields, "issn", record.issn.as_deref());
    push_optional(&mut fields, "language", record.language.as_deref());
    if let Some(arxiv) = record
        .arxiv_id
        .as_deref()
        .map(str::trim)
        .filter(|arxiv| !arxiv.is_empty())
    {
        push_field(&mut fields, "eprint", arxiv);
        push_field(&mut fields, "archiveprefix", "arXiv");
    }

    let mut out = String::new();
    out.push_str("@");
    out.push_str(entry_type);
    out.push('{');
    out.push_str(key);
    for (index, (name, value)) in fields.iter().enumerate() {
        // The first field opens with the key separator; later fields follow the
        // previous field's closing `},`.
        out.push_str(if index == 0 { ",\n  " } else { "\n  " });
        out.push_str(name);
        out.push_str(" = {");
        let value = escape_value(value, format);
        out.push_str(&value);
        if index + 1 < fields.len() {
            out.push_str("},");
        } else {
            out.push('}');
        }
    }
    out.push_str("\n}");
    out
}

fn push_field(fields: &mut Vec<(&'static str, String)>, name: &'static str, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        fields.push((name, value.to_string()));
    }
}

fn push_optional(fields: &mut Vec<(&'static str, String)>, name: &'static str, value: Option<&str>) {
    if let Some(value) = value {
        push_field(fields, name, value);
    }
}

/// `creators_json` first (`Last, First and Last, First`), then `authors`.
fn author_field(record: &PaperRecord) -> String {
    let from_creators = creators_authors(record.creators.as_ref());
    let names = if !from_creators.is_empty() {
        from_creators
    } else {
        record
            .authors
            .iter()
            .map(|author| display_author_to_bib(author))
            .filter(|author| !author.is_empty())
            .collect()
    };
    names.join(" and ")
}

fn creators_authors(creators: Option<&Value>) -> Vec<String> {
    let Some(creators) = creators.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut authors = Vec::new();
    for creator in creators {
        let creator_type = creator
            .get("creatorType")
            .and_then(Value::as_str)
            .unwrap_or("author");
        if !creator_type.is_empty() && !creator_type.eq_ignore_ascii_case("author") {
            continue;
        }
        let last = creator
            .get("lastName")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let first = creator
            .get("firstName")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !last.is_empty() {
            authors.push(if first.is_empty() {
                last.to_string()
            } else {
                format!("{last}, {first}")
            });
        } else if let Some(name) = creator
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            authors.push(name.to_string());
        }
    }
    authors
}

/// Convert a display name (`Ashish Vaswani`) to BibTeX order (`Vaswani, Ashish`).
/// Names already containing a comma are kept verbatim.
fn display_author_to_bib(author: &str) -> String {
    let author = author.trim();
    if author.is_empty() || author.contains(',') {
        return author.to_string();
    }
    match author.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] => String::new(),
        [only] => (*only).to_string(),
        parts => {
            let (surname, given) = parts.split_last().expect("non-empty");
            format!("{surname}, {}", given.join(" "))
        }
    }
}

/// `1-9` → `1--9` (BibTeX page range), keep existing `--`, normalize en dash.
fn normalize_pages(raw: &str) -> String {
    let trimmed = raw.trim().replace('–', "--");
    let chars: Vec<char> = trimmed.chars().collect();
    let mut out = String::with_capacity(trimmed.len() + 2);
    for (index, character) in chars.iter().enumerate() {
        if *character == '-' {
            let previous = index.checked_sub(1).and_then(|i| chars.get(i));
            let next = chars.get(index + 1);
            let already_double = previous == Some(&'-') || next == Some(&'-');
            if !already_double
                && previous.is_some_and(|c| c.is_ascii_digit())
                && next.is_some_and(|c| c.is_ascii_digit())
            {
                out.push('-');
                out.push('-');
                continue;
            }
        }
        out.push(*character);
    }
    out
}

/// BibTeX escape for plain-text field values (UTF-8 output is preserved).
///
/// BibLaTeX/Biber accepts UTF-8 and TeX constructs directly, so the biblatex
/// mount leaves values untouched; only the classic BibTeX dialect escapes.
fn escape_bibtex(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("\\&"),
            '%' => out.push_str("\\%"),
            '$' => out.push_str("\\$"),
            '#' => out.push_str("\\#"),
            '_' => out.push_str("\\_"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            '\\' => out.push_str("\\textbackslash{}"),
            other => out.push(other),
        }
    }
    out
}

/// Escape a value for the requested dialect before writing it into a field.
fn escape_value(value: &str, format: BibFormat) -> String {
    match format {
        BibFormat::Bibtex => escape_bibtex(value),
        BibFormat::Biblatex => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::paper::catalog::papers::PaperTag;
    use serde_json::json;

    fn record(path: &str, title: &str) -> PaperRecord {
        let mut record = PaperRecord::local_pdf(
            path.rsplit('/').next().unwrap_or("paper").to_string(),
            title.to_string(),
        )
        .at_path(path);
        record.authors = vec![];
        record
    }

    fn mount(tags: &[&str], dirs: &[&str]) -> BibtexMountSettings {
        BibtexMountSettings {
            enabled: true,
            format: "bibtex".into(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            dirs: dirs.iter().map(|dir| dir.to_string()).collect(),
        }
    }

    #[test]
    fn filters_by_tag_or_dir_prefix_and_and_combination() {
        let vault = tempfile::tempdir().unwrap();
        let mut alpha = record("papers/tag1/alpha", "Alpha");
        alpha.tags = vec![PaperTag::new("vision")];
        alpha.authors = vec!["Ashish Vaswani".into()];
        alpha.year = Some(2017);
        let mut tag10 = record("papers/tag10/beta", "Beta");
        tag10.tags = vec![PaperTag::new("nlp")];
        let mut plain = record("papers/plain", "Plain");
        plain.authors = vec!["Ada Lovelace".into()];
        for paper in [&alpha, &tag10, &plain] {
            papers::upsert_paper(vault.path(), paper).unwrap();
        }

        // Tag OR: either tag matches.
        let rendered = render(
            vault.path(),
            BibFormat::Bibtex,
            &mount(&["vision", "nlp"], &[]),
        )
        .unwrap();
        assert_eq!(rendered.count, 2);
        assert!(rendered.body.contains("@misc{vaswani2017,"));
        assert!(rendered.body.contains("@misc{beta,"));

        // Dir prefix: `papers/tag1` must not match `papers/tag10`.
        let rendered = render(vault.path(), BibFormat::Bibtex, &mount(&[], &["papers/tag1"])).unwrap();
        assert_eq!(rendered.count, 1);
        assert!(rendered.body.contains("Alpha"));

        // AND: tag match AND dir match.
        let rendered = render(
            vault.path(),
            BibFormat::Bibtex,
            &mount(&["vision"], &["papers/tag10"]),
        )
        .unwrap();
        assert_eq!(rendered.count, 0);
        assert_eq!(rendered.body, "");

        // Empty scope mounts everything.
        let rendered = render(vault.path(), BibFormat::Bibtex, &mount(&[], &[])).unwrap();
        assert_eq!(rendered.count, 3);
    }

    #[test]
    fn maps_entries_keys_and_fields() {
        let vault = tempfile::tempdir().unwrap();
        let mut attention = record("papers/attention", "Attention Is All You Need");
        attention.zotero_item_type = Some("journalArticle".into());
        attention.title = "Attention Is All You Need".into();
        attention.creators = Some(json!([
            {"creatorType": "author", "firstName": "Ashish", "lastName": "Vaswani"},
            {"creatorType": "author", "firstName": "Noam", "lastName": "Shazeer"},
        ]));
        attention.year = Some(2017);
        attention.publication = Some("NeurIPS".into());
        attention.pages = Some("5998-6008".into());
        attention.doi = Some("10.5555/3295222".into());
        papers::upsert_paper(vault.path(), &attention).unwrap();

        let mut conf = record("papers/conf", "A Conference Paper");
        conf.zotero_item_type = Some("conferencePaper".into());
        conf.authors = vec!["Ada Lovelace".into()];
        conf.publication = Some("Proc. of Testing".into());
        conf.year = Some(2020);
        papers::upsert_paper(vault.path(), &conf).unwrap();

        let rendered = render(vault.path(), BibFormat::Bibtex, &mount(&[], &[])).unwrap();
        assert_eq!(rendered.count, 2);
        assert!(rendered.body.contains("@article{vaswani2017,"));
        assert!(rendered.body.contains("author = {Vaswani, Ashish and Shazeer, Noam}"));
        assert!(rendered.body.contains("journal = {NeurIPS}"));
        assert!(rendered.body.contains("pages = {5998--6008}"));
        assert!(rendered.body.contains("doi = {10.5555/3295222}"));
        assert!(rendered.body.contains("@inproceedings{lovelace2020,"));
        assert!(rendered.body.contains("booktitle = {Proc. of Testing}"));
        assert!(rendered.body.contains("author = {Lovelace, Ada}"));
    }

    #[test]
    fn deduplicates_entry_keys() {
        let vault = tempfile::tempdir().unwrap();
        for folder in ["papers/a", "papers/b"] {
            let mut paper = record(folder, "Same Author Same Year");
            paper.authors = vec!["Ashish Vaswani".into()];
            paper.year = Some(2017);
            papers::upsert_paper(vault.path(), &paper).unwrap();
        }
        let rendered = render(vault.path(), BibFormat::Bibtex, &mount(&[], &[])).unwrap();
        assert_eq!(rendered.count, 2);
        assert!(rendered.body.contains("@misc{vaswani2017,"));
        assert!(rendered.body.contains("@misc{vaswani2017a,"));
    }

    #[test]
    fn bibtex_escapes_specials_and_keeps_utf8() {
        let vault = tempfile::tempdir().unwrap();
        let mut paper = record("papers/bits", "R&D 100% $x$ #1 _2 {Braces} ~ ^ \\");
        paper.authors = vec!["Ada Lovelace".into()];
        paper.year = Some(1843);
        papers::upsert_paper(vault.path(), &paper).unwrap();
        let rendered = render(vault.path(), BibFormat::Bibtex, &mount(&[], &[])).unwrap();
        assert!(rendered.body.contains("\\&"));
        assert!(rendered.body.contains("\\%"));
        assert!(rendered.body.contains("\\$"));
        assert!(rendered.body.contains("\\#"));
        assert!(rendered.body.contains("\\_"));
        assert!(rendered.body.contains("\\{Braces\\}"));
        assert!(rendered.body.contains("\\textasciitilde{}"));
        assert!(rendered.body.contains("\\textasciicircum{}"));
        assert!(rendered.body.contains("\\textbackslash{}"));

        // BibLaTeX keeps Unicode and TeX-valid characters verbatim.
        let mut cjk = record("papers/cjk", "注意力就是你所需要的一切");
        cjk.authors = vec!["张三".into()];
        cjk.year = Some(2024);
        papers::upsert_paper(vault.path(), &cjk).unwrap();
        let rendered = render(vault.path(), BibFormat::Biblatex, &mount(&[], &["papers/cjk"])).unwrap();
        assert!(rendered.body.contains("注意力就是你所需要的一切"));
        assert!(rendered.body.contains("@misc{cjk2024,"), "non-ASCII author key falls back to id");
    }

    #[test]
    fn biblatex_preprint_uses_online_and_eprint() {
        let vault = tempfile::tempdir().unwrap();
        let mut paper = record("papers/arxiv", "An arXiv Preprint");
        paper.zotero_item_type = Some("preprint".into());
        paper.arxiv_id = Some("2401.01234".into());
        paper.authors = vec!["Ada Lovelace".into()];
        paper.date = Some("2024-01-02".into());
        papers::upsert_paper(vault.path(), &paper).unwrap();
        let rendered = render(vault.path(), BibFormat::Biblatex, &mount(&[], &[])).unwrap();
        assert!(rendered.body.contains("@online{lovelace2024,"));
        assert!(rendered.body.contains("eprint = {2401.01234}"));
        assert!(rendered.body.contains("archiveprefix = {arXiv}"));
        assert!(rendered.body.contains("date = {2024-01-02}"));
    }

    #[test]
    fn pages_normalization() {
        assert_eq!(normalize_pages("1-9"), "1--9");
        assert_eq!(normalize_pages("1--9"), "1--9");
        assert_eq!(normalize_pages("1–9"), "1--9");
        assert_eq!(normalize_pages("e12345"), "e12345");
    }
}
