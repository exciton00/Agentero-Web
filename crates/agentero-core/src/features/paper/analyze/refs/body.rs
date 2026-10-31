//! Offline reference fallback: the numbered bibliography printed in the paper
//! body (`PAPER.md`).
//!
//! The TeX-source parsers ([`super::bbl`], [`super::bib`], [`super::latex`])
//! only cover papers imported with their LaTeX sources. Papers whose body was
//! produced by PDF layout analysis (MinerU/Paddle; the common Web case) usually
//! carry the bibliography as a numbered list in `PAPER.md` instead. This module
//! extracts those `[n]` entries and parses display metadata (title, authors,
//! year, venue, DOI/arXiv) straight from the raw citation text, so the
//! References panel has meaningful rows even with no network and no TeX.
//!
//! Online lookups stay authoritative: [`super::merge_online`] merges structured
//! Semantic Scholar/Crossref metadata on top of these local drafts.

use std::fs;
use std::path::Path;

use super::latex;
use super::RefDraft;

/// Body markdown written by the PDF body-parse pipeline.
pub const BODY_FILE: &str = "PAPER.md";
/// Guard: a pathological body must not be read into the parse thread.
const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;
/// Sanity cap on extracted entries (bibliographies are far smaller).
const MAX_ENTRIES: usize = 500;
/// Section headings that start a bibliography (case-insensitive, trailing
/// punctuation/emphasis stripped).
const HEADINGS: &[&str] = &[
    "references",
    "bibliography",
    "literature cited",
    "works cited",
    "参考文献",
];

/// Read and parse a `PAPER.md` path; empty when missing/unreadable/oversized.
pub fn parse_body_file(path: &Path) -> Vec<RefDraft> {
    let Ok(meta) = fs::metadata(path) else {
        return Vec::new();
    };
    if !meta.is_file() || meta.len() > MAX_BODY_BYTES {
        return Vec::new();
    }
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_body_refs(&text)
}

/// Extract `[n]`-numbered bibliography entries from paper markdown.
pub fn parse_body_refs(markdown: &str) -> Vec<RefDraft> {
    let Some(section) = references_section(markdown) else {
        return Vec::new();
    };

    let mut drafts: Vec<RefDraft> = Vec::new();
    let mut current: Option<(u32, String)> = None;
    // A non-numbered line continues the entry only when it belongs to the same
    // paragraph; a blank line closes the entry (author bios / footnotes after
    // the bibliography are never glued onto the last reference).
    let mut paragraph_open = false;

    for line in section.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush_entry(&mut current, &mut drafts);
            paragraph_open = false;
            continue;
        }
        if trimmed.starts_with('#') {
            // Next heading: bibliography is over.
            flush_entry(&mut current, &mut drafts);
            break;
        }
        if let Some((number, text)) = numbered_entry(trimmed) {
            flush_entry(&mut current, &mut drafts);
            current = Some((number, text));
            paragraph_open = true;
            continue;
        }
        if paragraph_open {
            if let Some((_, text)) = current.as_mut() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(trimmed);
            }
        }
    }
    flush_entry(&mut current, &mut drafts);
    drafts
}

/// Slice of `markdown` that starts after the bibliography heading.
fn references_section(markdown: &str) -> Option<&str> {
    let mut offset = 0usize;
    for line in markdown.split_inclusive('\n') {
        offset += line.len();
        if let Some(title) = heading_title(line.trim()) {
            if HEADINGS.contains(&title.as_str()) {
                return Some(&markdown[offset..]);
            }
        }
    }
    None
}

/// Heading text lowercased with markdown emphasis + trailing punctuation gone.
fn heading_title(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = trimmed[level..].trim();
    let title = rest
        .trim_matches(|c: char| c == '*' || c == '_' || c == ':' || c == ' ')
        .trim_end_matches(['.', ':', ' ']);
    let title = title.to_lowercase();
    (!title.is_empty()).then_some(title)
}

/// `"[12] Y. Wu, ..."` → `(12, "Y. Wu, ...")`.
fn numbered_entry(line: &str) -> Option<(u32, String)> {
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let number: u32 = rest[..close].trim().parse().ok()?;
    if number == 0 {
        return None;
    }
    let body = rest[close + 1..].trim_start();
    let body = body.strip_prefix(['.', ':']).unwrap_or(body).trim_start();
    Some((number, body.to_string()))
}

fn flush_entry(current: &mut Option<(u32, String)>, drafts: &mut Vec<RefDraft>) {
    let Some((number, text)) = current.take() else {
        return;
    };
    if drafts.len() >= MAX_ENTRIES {
        return;
    }
    let raw = strip_markup(&text);
    if raw.trim().is_empty() {
        return;
    }
    drafts.push(draft_from_raw_citation(number, raw));
}

// ---------------------------------------------------------------------------
// Raw citation text → display metadata
// ---------------------------------------------------------------------------

/// Build one local draft from a raw bibliography entry (IEEE-ish or generic).
pub(crate) fn draft_from_raw_citation(number: u32, raw: String) -> RefDraft {
    let mut draft = RefDraft {
        key: None,
        number: Some(number),
        raw: Some(raw.clone()),
        title: None,
        authors: Vec::new(),
        year: None,
        venue: None,
        doi: None,
        arxiv_id: None,
        url: None,
        source: "paper_md",
    };
    draft.doi = latex::extract_doi(&raw);
    draft.arxiv_id = latex::extract_arxiv_id(&raw)
        .as_deref()
        .map(latex::strip_arxiv_version)
        .map(str::to_string);
    draft.url = latex::extract_url(&raw);

    if let Some((title, authors, tail)) = split_quoted_title(&raw) {
        draft.title = non_empty(clean_title(&title));
        draft.authors = split_authors(&authors);
        let (venue, year) = parse_tail(&tail);
        draft.venue = venue;
        draft.year = year.or_else(|| latex::extract_year(&raw));
    } else {
        fill_from_unquoted(&raw, &mut draft);
    }
    draft
}

/// Split `Authors, “Title,” tail` into its three parts. Supports the common
/// curly/straight quote pairs; the first plausible title wins.
fn split_quoted_title(raw: &str) -> Option<(String, String, String)> {
    for (open, close) in [('“', '”'), ('"', '"'), ('「', '」')] {
        let Some(start) = raw.find(open) else {
            continue;
        };
        let after_open = start + open.len_utf8();
        let Some(rel_end) = raw[after_open..].find(close) else {
            continue;
        };
        let end = after_open + rel_end;
        let title = raw[after_open..end].trim();
        if title.chars().filter(|c| c.is_alphabetic()).count() < 4 {
            continue;
        }
        let authors = raw[..start].trim().trim_end_matches([',', '.', ';', ' ']);
        let tail = raw[end + close.len_utf8()..]
            .trim()
            .trim_start_matches([',', '.', ';', ' ']);
        return Some((title.to_string(), authors.to_string(), tail.to_string()));
    }
    None
}

fn clean_title(title: &str) -> String {
    title
        .trim()
        .trim_end_matches(['.', ',', ';', ':', ' '])
        .to_string()
}

/// `year` + `venue` from the text after the title.
fn parse_tail(tail: &str) -> (Option<String>, Option<i32>) {
    let year = latex::extract_year(tail);
    let mut venue_parts: Vec<&str> = Vec::new();
    for part in tail.split(',') {
        let part = part.trim();
        if part.is_empty() || is_bibliographic_stop(part) {
            break;
        }
        venue_parts.push(part);
        if venue_parts.len() >= 2 {
            break;
        }
    }
    let venue = non_empty(
        venue_parts
            .join(", ")
            .trim()
            .trim_start_matches("in ")
            .trim_start_matches("In ")
            // Keep abbreviation periods (`IEDM Tech. Dig.`); only drop list
            // separators.
            .trim_end_matches([',', ';', ' '])
            .to_string(),
    );
    (venue, year)
}

/// Elements that end the venue segment (`vol. 12`, `pp. 3–4`, `2007`, URLs…).
fn is_bibliographic_stop(part: &str) -> bool {
    let lower = part.to_ascii_lowercase();
    if lower.starts_with("vol")
        || lower.starts_with("no.")
        || lower.starts_with("no ")
        || lower.starts_with("pp")
        || lower.starts_with("p.")
        || lower.starts_with("page")
        || lower.starts_with("doi")
        || lower.starts_with("http")
        || lower.starts_with("arxiv")
        || lower.contains("http")
    {
        return true;
    }
    // Standalone year token ("2007", "2007.").
    let token = part.trim_end_matches('.').trim();
    token.len() == 4
        && token.chars().all(|c| c.is_ascii_digit())
        && (1900..=2099).contains(&token.parse::<i32>().unwrap_or(0))
}

/// Fallback for unquoted styles (`A. Author, Title, Venue, year.`):
/// pick the first comma segment that reads like a title, keep the segments
/// before it as authors and the next non-numeric one as the venue.
fn fill_from_unquoted(raw: &str, draft: &mut RefDraft) {
    let parts: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() < 3 {
        return;
    }
    let mut title_idx = None;
    for (index, part) in parts.iter().enumerate().skip(1) {
        if is_bibliographic_stop(part) {
            continue;
        }
        let words = part.split_whitespace().count();
        let has_word = part
            .split_whitespace()
            .any(|word| word.len() > 2 && word.chars().any(char::is_lowercase));
        if words >= 3 && has_word {
            title_idx = Some(index);
            break;
        }
    }
    let Some(title_idx) = title_idx else {
        return;
    };
    let authors = split_authors(&parts[..title_idx].join(", "));
    if authors.is_empty() {
        return;
    }
    let title = clean_title(parts[title_idx]);
    if title.chars().filter(|c| c.is_alphabetic()).count() < 4 {
        return;
    }
    draft.title = Some(title);
    draft.authors = authors;
    for part in parts.iter().skip(title_idx + 1) {
        if is_bibliographic_stop(part) {
            continue;
        }
        let venue = part
            .trim_start_matches("in ")
            .trim_start_matches("In ")
            .trim_end_matches([',', ';', ' ']);
        if !venue.is_empty() {
            draft.venue = Some(venue.to_string());
            break;
        }
    }
    draft.year = latex::extract_year(raw);
}

/// Split an author segment into display names. Handles `A. B, C. D and E. F`
/// and `;`-separated lists; drops empty / "and" fragments.
fn split_authors(text: &str) -> Vec<String> {
    let text = text.trim().trim_end_matches(['.', ',', ';', ' ']);
    if text.is_empty() {
        return Vec::new();
    }
    let mut authors = Vec::new();
    for chunk in text.split(';') {
        for piece in chunk.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let without_and = piece
                .strip_prefix("and ")
                .or_else(|| piece.strip_prefix("And "))
                .unwrap_or(piece)
                .trim();
            let without_and = without_and.strip_prefix('&').unwrap_or(without_and).trim();
            let name = without_and.trim_matches(['.', ',', ';', ' ']).trim();
            if name.is_empty() || name.eq_ignore_ascii_case("and") {
                continue;
            }
            authors.push(name.to_string());
            if authors.len() >= 40 {
                return authors;
            }
        }
    }
    authors
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

// ---------------------------------------------------------------------------
// Markup cleanup
// ---------------------------------------------------------------------------

/// Strip HTML tags/entities and markdown emphasis from a raw reference line.
fn strip_markup(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(pos) = rest.find(['<', '&']) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if tail.starts_with('<') {
            // Only treat it as a tag when it looks like one; a lone `<` (e.g.
            // a comparison symbol) is kept as text.
            let Some(close) = tail.find('>') else {
                out.push_str(tail);
                return crate::features::paper::util::collapse_ws(&out);
            };
            let inner = &tail[1..close];
            let looks_like_tag = !inner.is_empty()
                && inner.len() <= 24
                && inner.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '/' | '=' | '"' | '\'' | '-' | ' ' | ':')
                })
                && inner
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '/');
            rest = &tail[close + 1..];
            if looks_like_tag {
                continue;
            }
            out.push_str(&tail[..=close]);
            continue;
        }
        // Entity: `&amp;` / `&#x2013;` / numeric.
        if let Some(semi) = tail[1..].find(';').map(|i| i + 1) {
            if semi <= 12 {
                if let Some(ch) = decode_entity(&tail[1..semi]) {
                    out.push(ch);
                    rest = &tail[semi + 1..];
                    continue;
                }
            }
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    crate::features::paper::util::collapse_ws(&out)
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity.to_ascii_lowercase().as_str() {
        "amp" => return Some('&'),
        "lt" => return Some('<'),
        "gt" => return Some('>'),
        "quot" => return Some('"'),
        "apos" | "#39" => return Some('\''),
        "nbsp" => return Some(' '),
        "ndash" => return Some('–'),
        "mdash" => return Some('—'),
        _ => {}
    }
    let number = entity.strip_prefix('#')?;
    let value = if let Some(hex) = number.strip_prefix(['x', 'X']) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        number.parse::<u32>().ok()?
    };
    char::from_u32(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IEEE_REFS: &str = "## REFERENCES

[1] Y. F. Wu, M. Moore, A. Abrahamsen, M. Jacob-Mitos, P. Parikh, S. Heikman, and A. Burk, “High-voltage millimeter-wave GaN HEMTs with 13.7 W/mm power density,” in IEDM Tech. Dig., 2007, pp. 405–407.

[2] S. C. Binari, P. B. Klein, and T. E. Kazior, “Trapping effects in GaN and SiC microwave FETs,” Proc. IEEE, vol. 90, no. 6, pp. 1048–1058, Jun. 2002.

**Jungwoo Joh** received the B.S. degree from Seoul National University in 2002.
";

    #[test]
    fn parses_numbered_ieee_entries_offline() {
        let drafts = parse_body_refs(IEEE_REFS);
        assert_eq!(drafts.len(), 2, "author bio is not a reference");

        let first = &drafts[0];
        assert_eq!(first.number, Some(1));
        assert_eq!(first.source, "paper_md");
        assert_eq!(
            first.title.as_deref(),
            Some("High-voltage millimeter-wave GaN HEMTs with 13.7 W/mm power density")
        );
        assert_eq!(first.authors.len(), 7);
        assert_eq!(first.authors[0], "Y. F. Wu");
        assert_eq!(first.authors[6], "A. Burk");
        assert_eq!(first.year, Some(2007));
        assert_eq!(first.venue.as_deref(), Some("IEDM Tech. Dig."));
        assert!(first
            .raw
            .as_deref()
            .is_some_and(|r| r.contains("13.7 W/mm")));

        let second = &drafts[1];
        assert_eq!(second.number, Some(2));
        assert_eq!(
            second.title.as_deref(),
            Some("Trapping effects in GaN and SiC microwave FETs")
        );
        assert_eq!(second.venue.as_deref(), Some("Proc. IEEE"));
        assert_eq!(second.year, Some(2002));
    }

    #[test]
    fn strips_html_tags_and_entities_from_raw_text() {
        let markdown = "## References\n\n[1] G. Koley, “Slow transients in SiN<sub>x</sub> HFETs,” IEEE Trans. Electron Devices, vol. 50, 2003.\n";
        let drafts = parse_body_refs(markdown);
        assert_eq!(drafts.len(), 1);
        let raw = drafts[0].raw.as_deref().unwrap_or_default();
        assert!(raw.contains("SiNx"), "raw: {raw}");
        assert!(!raw.contains('<'));
        assert_eq!(
            drafts[0].title.as_deref(),
            Some("Slow transients in SiNx HFETs")
        );
        assert_eq!(drafts[0].year, Some(2003));
    }

    #[test]
    fn joined_wrapped_lines_belong_to_the_same_entry() {
        let markdown = "## References\n\n[1] A. Author, B. Author, “A very long title that wraps\nacross two physical lines,” in Proc. X, 2020, pp. 1–2.\n";
        let drafts = parse_body_refs(markdown);
        assert_eq!(drafts.len(), 1);
        assert_eq!(
            drafts[0].title.as_deref(),
            Some("A very long title that wraps across two physical lines")
        );
        assert_eq!(drafts[0].year, Some(2020));
    }

    #[test]
    fn parses_unquoted_comma_style_as_fallback() {
        let draft = draft_from_raw_citation(
            3,
            "A. Author, B. Author, An offline parsing approach for references, Proc. ACM, 2019."
                .to_string(),
        );
        assert_eq!(
            draft.title.as_deref(),
            Some("An offline parsing approach for references")
        );
        assert_eq!(
            draft.authors,
            vec!["A. Author".to_string(), "B. Author".to_string()]
        );
        assert_eq!(draft.venue.as_deref(), Some("Proc. ACM"));
        assert_eq!(draft.year, Some(2019));
    }

    #[test]
    fn extracts_identifiers_and_links_from_raw_text() {
        let draft = draft_from_raw_citation(
            4,
            "J. Doe, “Some title,” in Proc. Y, 2021. doi:10.1109/ABC.2021.1 arXiv:2101.00001v2 https://example.org/x"
                .to_string(),
        );
        assert_eq!(draft.doi.as_deref(), Some("10.1109/ABC.2021.1"));
        assert_eq!(draft.arxiv_id.as_deref(), Some("2101.00001"));
        assert_eq!(draft.url.as_deref(), Some("https://example.org/x"));
    }

    #[test]
    fn empty_without_a_bibliography_heading() {
        assert!(parse_body_refs("# Title\n\nBody text without references.").is_empty());
        assert!(parse_body_refs("## References\n\nNo numbered entries here.").is_empty());
    }

    #[test]
    fn accepts_chinese_and_alternate_heading_names() {
        let zh = "## 参考文献\n\n[1] 王小明, “论文标题,” 计算机学报, 2020.\n";
        let drafts = parse_body_refs(zh);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].title.as_deref(), Some("论文标题"));

        let bibliography =
            "# Paper\n\n## Bibliography\n\n[1] A. Author, “Title of the work,” Venue, 2018.\n";
        assert_eq!(parse_body_refs(bibliography).len(), 1);
    }

    #[test]
    fn later_section_heading_ends_the_bibliography() {
        let markdown = "## References\n\n[1] A. Author, “First title here,” Venue, 2018.\n\n## Appendix\n\n[2] Not a reference, “also not” 2021.\n";
        let drafts = parse_body_refs(markdown);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].number, Some(1));
    }
}
