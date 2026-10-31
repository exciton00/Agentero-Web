//! Read-only PDF document anchors (numbered sections + the body paragraphs
//! they own) for the MCP `layout_list` / `layout_get` tools.
//!
//! Ported from upstream Agentero 0.14
//! (`crates/agentero-core/src/features/pdf/{layout_index,document_reading_order}.rs`,
//! commits `7e89cd52` / `a67693f1`). The shared core layout index is not
//! modified; this module only reads `{paper}/source/layout.json`.
//!
//! Anchors are the fragments users can paste onto a paper path
//! (`paper.pdf#2.1#p3`): `#2.1` addresses a numbered heading and `#2.1#p3` the
//! third body block owned by that heading. Numbers are only assigned to
//! numbered headings; unnumbered headings still end the preceding section so
//! their body never leaks into it. Duplicate section numbers make the affected
//! anchors ambiguous and are reported instead of guessed.

use agentero_core::error::AppError;
use agentero_core::features::pdf::layout_index::LAYOUT_RAW_FILE;
use agentero_core::fs::sanitize_vault_rel;
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

/// Normalized (`0..1`) page box, matching the raw `layout.json` `bbox` field.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AnchorBbox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone)]
struct AnchorRegion {
    id: String,
    page_index: u32,
    kind: String,
    title: Option<String>,
    text: Option<String>,
    bbox: AnchorBbox,
    reading_order: Option<u32>,
    score: f64,
}

/// One section or paragraph anchor with its original text and coordinates.
#[derive(Debug, Clone)]
pub struct DocumentAnchor {
    pub id: String,
    /// `section` or `paragraph`.
    pub kind: String,
    /// `#2.1` or `#2.1#p3`; `None` when the owning section number is ambiguous.
    pub anchor: Option<String>,
    /// `#2.1`; `None` after an unnumbered heading.
    pub section_anchor: Option<String>,
    pub paragraph_index: Option<u32>,
    pub title: Option<String>,
    pub text: Option<String>,
    /// 1-based physical page.
    pub page: u32,
    /// 0-based physical page index.
    pub page_index: u32,
    pub bbox: AnchorBbox,
    pub score: f64,
}

/// Read `{paper}/source/layout.json` and enumerate its document anchors.
pub fn load(vault: &Path, paper_path: &str) -> Result<Vec<DocumentAnchor>, AppError> {
    let regions = read_regions(vault, paper_path)?;
    Ok(document_anchors(&regions))
}

/// Resolve an exact published fragment (`#2.1` / `2.1#p3` / `#2.1#p3`).
/// Missing or ambiguous anchors error instead of falling back to a guess.
pub fn resolve(vault: &Path, paper_path: &str, fragment: &str) -> Result<DocumentAnchor, AppError> {
    let anchors = load(vault, paper_path)?;
    resolve_in(&anchors, fragment)
}

fn resolve_in(anchors: &[DocumentAnchor], fragment: &str) -> Result<DocumentAnchor, AppError> {
    let anchor = format!("#{}", fragment.strip_prefix('#').unwrap_or(fragment));
    let section = anchor.split("#p").next().unwrap_or(&anchor);
    if anchors
        .iter()
        .filter(|a| a.kind == "section" && a.anchor.as_deref() == Some(section))
        .count()
        > 1
    {
        return Err(AppError::domain(
            "citation_anchor_ambiguous",
            format!("ambiguous PDF section `{section}`"),
        ));
    }
    let mut matches = anchors
        .iter()
        .filter(|a| a.anchor.as_deref() == Some(anchor.as_str()))
        .cloned();
    let first = matches.next();
    match (first, matches.next()) {
        (Some(item), None) => Ok(item),
        (None, _) => Err(AppError::domain(
            "citation_anchor_not_found",
            format!("no PDF anchor `{anchor}`"),
        )),
        (Some(_), Some(_)) => Err(AppError::domain(
            "citation_anchor_ambiguous",
            format!("ambiguous PDF anchor `{anchor}`"),
        )),
    }
}

fn paper_source_path(vault: &Path, paper_path: &str) -> Result<std::path::PathBuf, AppError> {
    let rel = sanitize_vault_rel(paper_path).map_err(AppError::message)?;
    Ok(vault.join(rel).join("source").join(LAYOUT_RAW_FILE))
}

fn read_regions(vault: &Path, paper_path: &str) -> Result<Vec<AnchorRegion>, AppError> {
    let path = paper_source_path(vault, paper_path)?;
    if !path.is_file() {
        return Err(AppError::domain(
            "layout_raw_missing",
            format!(
                "{paper_path}/source/{LAYOUT_RAW_FILE} not found; open the paper and run layout analysis first"
            ),
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| AppError::message(format!("failed to read raw layout: {e}")))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| AppError::message(format!("invalid raw layout json: {e}")))?;
    let arr = value
        .get("regions")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AppError::domain("layout_raw_invalid", "layout.json missing regions array")
        })?;
    let mut regions = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        regions.push(parse_region(entry).ok_or_else(|| {
            AppError::domain(
                "layout_raw_invalid",
                format!("invalid raw layout region at index {i}"),
            )
        })?);
    }
    Ok(regions)
}

fn parse_region(v: &Value) -> Option<AnchorRegion> {
    let id = v.get("id")?.as_str()?.to_string();
    let page_index = v.get("pageIndex")?.as_u64()? as u32;
    let kind = v.get("kind")?.as_str()?.to_string();
    if id.is_empty() || kind.is_empty() {
        return None;
    }
    let bbox_v = v.get("bbox")?;
    let bbox = AnchorBbox {
        x: bbox_v.get("x")?.as_f64()?,
        y: bbox_v.get("y")?.as_f64()?,
        w: bbox_v.get("w")?.as_f64()?,
        h: bbox_v.get("h")?.as_f64()?,
    };
    let reading_order = v
        .get("readingOrder")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    let score = v.get("score").and_then(Value::as_f64).unwrap_or(1.0);
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let text = v
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    Some(AnchorRegion {
        id,
        page_index,
        kind,
        title,
        text,
        bbox,
        reading_order,
        score,
    })
}

fn stored_order(a: &AnchorRegion, b: &AnchorRegion) -> Ordering {
    a.reading_order
        .unwrap_or(u32::MAX)
        .cmp(&b.reading_order.unwrap_or(u32::MAX))
        .then_with(|| a.bbox.y.total_cmp(&b.bbox.y))
        .then_with(|| a.bbox.x.total_cmp(&b.bbox.x))
        .then_with(|| a.id.cmp(&b.id))
}

fn has_valid_box(r: &AnchorRegion) -> bool {
    let b = &r.bbox;
    [b.x, b.y, b.w, b.h].iter().all(|v| v.is_finite()) && b.w > 0.0 && b.h > 0.0
}

/// Repair y-only provider order for columnar pages.
///
/// Remote layout producers can store `readingOrder` in pure y order, which
/// interleaves the columns. A gutter is inferred from disjoint body-text x
/// intervals with body text on both sides overlapping vertically; short or
/// inset headings are not evidence of a second column. Full-width blocks
/// delimit horizontal bands. Keep in sync with the upstream
/// `document_reading_order.rs` / `document-reading-order.ts` implementations.
fn page_order<'a>(page: &[&'a AnchorRegion]) -> Vec<&'a AnchorRegion> {
    if !page.iter().all(|r| has_valid_box(r)) {
        return page.to_vec();
    }
    let mut body: Vec<_> = page
        .iter()
        .copied()
        .filter(|r| {
            matches!(r.kind.as_str(), "text" | "paragraph")
                && r.text.as_deref().is_some_and(|s| !s.trim().is_empty())
                && r.bbox.w <= 0.6
        })
        .collect();
    body.sort_by(|a, b| a.bbox.x.total_cmp(&b.bbox.x));
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    for r in &body {
        let b = &r.bbox;
        if let Some(last) = intervals.last_mut().filter(|last| b.x - last.1 < 0.01) {
            last.1 = last.1.max(b.x + b.w);
        } else {
            intervals.push((b.x, b.x + b.w));
        }
    }
    let mut gutter = None;
    let mut gap = 0.0;
    for pair in intervals.windows(2) {
        let width = pair[1].0 - pair[0].1;
        if width <= gap {
            continue;
        }
        let mid = (pair[0].1 + pair[1].0) / 2.0;
        let overlaps = body.iter().filter(|a| a.bbox.x + a.bbox.w <= mid).any(|a| {
            body.iter()
                .filter(|b| b.bbox.x >= mid)
                .any(|b| (a.bbox.y + a.bbox.h).min(b.bbox.y + b.bbox.h) > a.bbox.y.max(b.bbox.y))
        });
        if overlaps {
            gutter = Some(mid);
            gap = width;
        }
    }
    let Some(mid) = gutter else {
        return page.to_vec();
    };
    let spans_gutter = |r: &&AnchorRegion| r.bbox.x < mid && r.bbox.x + r.bbox.w > mid;
    let mut spans: Vec<_> = page.iter().copied().filter(spans_gutter).collect();
    spans.sort_by(|a, b| {
        a.bbox
            .y
            .total_cmp(&b.bbox.y)
            .then_with(|| stored_order(a, b))
    });
    let mut remaining: Vec<_> = page.iter().copied().filter(|r| !spans_gutter(r)).collect();
    let columns = |a: &&AnchorRegion, b: &&AnchorRegion| {
        (a.bbox.x >= mid)
            .cmp(&(b.bbox.x >= mid))
            .then_with(|| a.bbox.y.total_cmp(&b.bbox.y))
            .then_with(|| stored_order(a, b))
    };
    let mut ordered = Vec::with_capacity(page.len());
    for span in spans {
        let (mut before, after): (Vec<_>, Vec<_>) =
            remaining.into_iter().partition(|r| r.bbox.y < span.bbox.y);
        before.sort_by(columns);
        ordered.extend(before);
        ordered.push(span);
        remaining = after;
    }
    remaining.sort_by(columns);
    ordered.extend(remaining);
    ordered
}

fn document_reading_order(regions: &[AnchorRegion]) -> Vec<&AnchorRegion> {
    let mut sorted: Vec<_> = regions.iter().collect();
    sorted.sort_by(|a, b| {
        a.page_index
            .cmp(&b.page_index)
            .then_with(|| stored_order(a, b))
    });
    let mut ordered = Vec::with_capacity(sorted.len());
    let mut start = 0;
    while start < sorted.len() {
        let end = start
            + sorted[start..]
                .iter()
                .take_while(|r| r.page_index == sorted[start].page_index)
                .count();
        ordered.extend(page_order(&sorted[start..end]));
        start = end;
    }
    ordered
}

/// Explicit Arabic hierarchy or an IEEE Roman top-level marker. Never fuzzy
/// match a numeric substring (`2` must not resolve to `2.1`, `12` or an arXiv id).
fn section_number(title: &str) -> Option<String> {
    let trimmed = title.trim_start();
    let numeric: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let number = numeric.trim_end_matches('.');
    let boundary = trimmed.get(numeric.len()..).unwrap_or("");
    let boundary_ok = boundary.is_empty()
        || boundary
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == ':' || c == ')');
    if !number.is_empty()
        && (boundary_ok || numeric.ends_with('.'))
        && number
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Some(
            number
                .split('.')
                .map(|p| p.parse::<u32>().ok().map(|n| n.to_string()))
                .collect::<Option<Vec<_>>>()?
                .join("."),
        );
    }
    // Try compact dotted OCR markers, then a normal whitespace-delimited
    // marker. A later dot in the heading text is not a marker boundary.
    const ROMAN: [&str; 20] = [
        "i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x", "xi", "xii", "xiii", "xiv",
        "xv", "xvi", "xvii", "xviii", "xix", "xx",
    ];
    let roman_number = |marker: &str| {
        ROMAN
            .iter()
            .position(|r| *r == marker.to_ascii_lowercase())
            .map(|n| (n + 1).to_string())
    };
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some((marker, _)) = compact.split_once('.') {
        if let Some(number) = roman_number(marker) {
            return Some(number);
        }
    }
    roman_number(
        trimmed
            .split_whitespace()
            .next()?
            .trim_end_matches(['.', ':', ')']),
    )
}

fn enumerate_document_anchors(regions: &[AnchorRegion]) -> Vec<DocumentAnchor> {
    let mut items = Vec::new();
    let mut section_anchor: Option<String> = None;
    let mut paragraph_index = 0u32;
    for region in document_reading_order(regions) {
        let (kind, anchor, ordinal) = if region.kind == "header" {
            section_anchor = region
                .title
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .or(region.text.as_deref())
                .and_then(section_number);
            paragraph_index = 0;
            ("section", section_anchor.clone(), None)
        } else if matches!(region.kind.as_str(), "text" | "paragraph")
            && region.text.as_deref().is_some_and(|s| !s.trim().is_empty())
        {
            let Some(section) = section_anchor.as_ref() else {
                continue;
            };
            paragraph_index += 1;
            (
                "paragraph",
                Some(format!("{section}#p{paragraph_index}")),
                Some(paragraph_index),
            )
        } else {
            continue;
        };
        items.push(DocumentAnchor {
            id: region.id.clone(),
            kind: kind.to_string(),
            anchor: anchor.map(|s| format!("#{s}")),
            section_anchor: section_anchor.as_ref().map(|s| format!("#{s}")),
            paragraph_index: ordinal,
            title: region.title.clone(),
            text: region.text.clone(),
            page: region.page_index + 1,
            page_index: region.page_index,
            bbox: region.bbox,
            score: region.score,
        });
    }
    items
}

/// Numbered headings and directly owned body blocks, before any score filter.
fn document_anchors(regions: &[AnchorRegion]) -> Vec<DocumentAnchor> {
    let mut items = enumerate_document_anchors(regions);
    let mut counts: HashMap<String, usize> = HashMap::new();
    for item in &items {
        if item.kind == "section" {
            if let Some(anchor) = &item.anchor {
                *counts.entry(anchor.clone()).or_insert(0) += 1;
            }
        }
    }
    for item in &mut items {
        if item
            .section_anchor
            .as_ref()
            .is_some_and(|a| counts.get(a).copied().unwrap_or(0) > 1)
        {
            item.anchor = None;
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(id: &str, page: u32, kind: &str, x: f64, y: f64, w: f64, h: f64) -> AnchorRegion {
        AnchorRegion {
            id: id.into(),
            page_index: page,
            kind: kind.into(),
            title: None,
            text: None,
            bbox: AnchorBbox { x, y, w, h },
            reading_order: None,
            score: 1.0,
        }
    }

    fn header(id: &str, title: &str) -> AnchorRegion {
        header_at(id, title, 0.05)
    }

    fn header_at(id: &str, title: &str, y: f64) -> AnchorRegion {
        AnchorRegion {
            title: Some(title.into()),
            ..region(id, 0, "header", 0.1, y, 0.8, 0.03)
        }
    }

    fn paragraph(id: &str, text: &str, y: f64) -> AnchorRegion {
        AnchorRegion {
            text: Some(text.into()),
            ..region(id, 0, "text", 0.1, y, 0.8, 0.05)
        }
    }

    #[test]
    fn section_number_recognizes_numbered_and_roman_headings() {
        assert_eq!(section_number("2 Introduction"), Some("2".into()));
        assert_eq!(section_number("2.1 Method"), Some("2.1".into()));
        assert_eq!(section_number("2.1.3 Details"), Some("2.1.3".into()));
        assert_eq!(section_number("3. Results"), Some("3".into()));
        assert_eq!(section_number("2.1: Setup"), Some("2.1".into()));
        assert_eq!(section_number("2.1) Setup"), Some("2.1".into()));
        assert_eq!(section_number("04 Overview"), Some("4".into()));
        assert_eq!(section_number("II. Related Work"), Some("2".into()));
        assert_eq!(section_number("III: Results"), Some("3".into()));
        assert_eq!(section_number("II.Related Work"), Some("2".into()));
        assert_eq!(section_number("1 Introduction"), Some("1".into()));
        // No number: a later dot in the heading text is not a marker boundary.
        assert_eq!(section_number("Introduction"), None);
        assert_eq!(section_number("Attention, please"), None);
        assert_eq!(section_number("Appendix A"), None);
        // Only the exact leading marker counts; a numeric substring does not.
        assert_eq!(section_number("12"), Some("12".into()));
    }

    #[test]
    fn document_anchors_own_paragraphs_until_an_unnumbered_heading() {
        let regions = vec![
            header("h1", "1 Introduction"),
            paragraph("p1", "first body block", 0.1),
            paragraph("p2", "second body block", 0.2),
            header_at("h2", "2 Method", 0.25),
            paragraph("p3", "method body", 0.3),
        ];
        let anchors = document_anchors(&regions);
        assert_eq!(
            anchors
                .iter()
                .map(|a| a.anchor.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("#1"),
                Some("#1#p1"),
                Some("#1#p2"),
                Some("#2"),
                Some("#2#p1")
            ]
        );
        assert_eq!(anchors[2].paragraph_index, Some(2));
        assert_eq!(anchors[4].section_anchor.as_deref(), Some("#2"));

        // An unnumbered heading ends ownership: its body blocks get no anchor.
        // The heading itself is still listed as a section with `anchor: None`.
        let regions = vec![
            header("h1", "1 Introduction"),
            paragraph("p1", "intro body", 0.1),
            header_at("h2", "Acknowledgements", 0.2),
            paragraph("p2", "thanks", 0.3),
        ];
        let anchors = document_anchors(&regions);
        assert_eq!(anchors.len(), 3);
        assert_eq!(anchors[0].anchor.as_deref(), Some("#1"));
        assert_eq!(anchors[1].anchor.as_deref(), Some("#1#p1"));
        assert_eq!(anchors[2].kind, "section");
        assert_eq!(anchors[2].anchor, None);
        assert_eq!(anchors[2].section_anchor, None);
    }

    #[test]
    fn duplicate_section_numbers_make_anchors_ambiguous() {
        let regions = vec![
            header("h1", "1 Introduction"),
            paragraph("p1", "first", 0.1),
            header_at("h2", "1 Introduction", 0.15),
            paragraph("p2", "second", 0.2),
        ];
        let anchors = document_anchors(&regions);
        assert!(
            anchors.iter().all(|a| a.anchor.is_none()),
            "duplicate section numbers must not publish anchors: {anchors:?}"
        );
        assert_eq!(
            resolve_in(&anchors, "#1#p1").unwrap_err().code(),
            "citation_anchor_not_found"
        );
    }

    #[test]
    fn resolve_rejects_missing_fragments() {
        let regions = vec![header("h1", "1 Introduction"), paragraph("p1", "body", 0.1)];
        let anchors = document_anchors(&regions);
        let item = resolve_in(&anchors, "1#p1").unwrap();
        assert_eq!(item.kind, "paragraph");
        assert_eq!(item.text.as_deref(), Some("body"));
        let item = resolve_in(&anchors, "#1").unwrap();
        assert_eq!(item.kind, "section");
        assert_eq!(
            resolve_in(&anchors, "#2").unwrap_err().code(),
            "citation_anchor_not_found"
        );
    }

    #[test]
    fn reading_order_repairs_y_only_column_order() {
        // Header spans the page, then two columns whose provider readingOrder
        // is y-only (right column blocks interleaved before left column).
        let regions = vec![
            AnchorRegion {
                title: Some("1 Introduction".into()),
                ..region("h1", 0, "header", 0.1, 0.01, 0.8, 0.02)
            },
            AnchorRegion {
                text: Some("left first".into()),
                reading_order: Some(2),
                ..region("l1", 0, "text", 0.1, 0.1, 0.3, 0.1)
            },
            AnchorRegion {
                text: Some("right first".into()),
                reading_order: Some(1),
                ..region("r1", 0, "text", 0.55, 0.1, 0.3, 0.1)
            },
            AnchorRegion {
                text: Some("left second".into()),
                reading_order: Some(4),
                ..region("l2", 0, "text", 0.1, 0.3, 0.3, 0.1)
            },
            AnchorRegion {
                text: Some("right second".into()),
                reading_order: Some(3),
                ..region("r2", 0, "text", 0.55, 0.3, 0.3, 0.1)
            },
        ];
        let order: Vec<&str> = document_reading_order(&regions)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(order, vec!["h1", "l1", "l2", "r1", "r2"]);
        // Paragraph ordinals follow the repaired order, not the provider order.
        let anchors = document_anchors(&regions);
        assert_eq!(anchors[1].text.as_deref(), Some("left first"));
        assert_eq!(anchors[2].text.as_deref(), Some("left second"));
        assert_eq!(anchors[3].text.as_deref(), Some("right first"));
        assert_eq!(anchors[3].anchor.as_deref(), Some("#1#p3"));
    }

    fn write_layout(root: &Path, json: &str) {
        let source = root.join("papers/demo/source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join(LAYOUT_RAW_FILE), json).unwrap();
    }

    #[test]
    fn load_and_resolve_read_layout_json() {
        let vault = tempfile::tempdir().unwrap();
        write_layout(
            vault.path(),
            r#"{
              "schemaVersion": 3,
              "regions": [
                {"id": "h1", "pageIndex": 0, "kind": "header", "score": 0.9,
                 "bbox": {"x": 0.1, "y": 0.05, "w": 0.8, "h": 0.03}, "title": "2 Method"},
                {"id": "p1", "pageIndex": 0, "kind": "paragraph", "score": 0.8,
                 "bbox": {"x": 0.1, "y": 0.1, "w": 0.8, "h": 0.05}, "text": "First paragraph."},
                {"id": "p2", "pageIndex": 1, "kind": "text", "score": 0.7, "readingOrder": 1,
                 "bbox": {"x": 0.1, "y": 0.1, "w": 0.8, "h": 0.05}, "text": "Second paragraph."}
              ]
            }"#,
        );
        let anchors = load(vault.path(), "papers/demo").unwrap();
        assert_eq!(anchors.len(), 3);
        assert_eq!(anchors[0].anchor.as_deref(), Some("#2"));
        let item = resolve(vault.path(), "papers/demo", "#2#p2").unwrap();
        assert_eq!(item.text.as_deref(), Some("Second paragraph."));
        assert_eq!(item.page, 2);
        assert_eq!(item.page_index, 1);
        assert_eq!(
            resolve(vault.path(), "papers/demo", "#9")
                .unwrap_err()
                .code(),
            "citation_anchor_not_found"
        );
    }

    #[test]
    fn load_rejects_missing_and_invalid_layout() {
        let vault = tempfile::tempdir().unwrap();
        assert_eq!(
            load(vault.path(), "papers/demo").unwrap_err().code(),
            "layout_raw_missing"
        );
        write_layout(vault.path(), r#"{"regions":[{"id":"x"}]}"#);
        assert_eq!(
            load(vault.path(), "papers/demo").unwrap_err().code(),
            "layout_raw_invalid"
        );
        // Path traversal is rejected by the shared vault-rel sanitizer.
        assert!(load(vault.path(), "../etc").is_err());
    }
}
