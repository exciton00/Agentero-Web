//! MCP layout_list / layout_get helpers + schemars DTOs.

use super::{anchors, paper};
use agentero_core::error::AppError;
use agentero_core::features::pdf::layout_index::{
    self, LayoutGetResult, LayoutIndexItem, LayoutListResult,
};
use serde::Serialize;
use std::path::Path;

/// Kinds served by the sidebar `layout-index.json`; everything else that
/// `layout_list` accepts is read from the raw `layout.json` document anchors.
const INDEX_KINDS: [&str; 6] = ["figure", "image", "chart", "table", "algorithm", "formula"];
const ANCHOR_KINDS: [&str; 2] = ["section", "paragraph"];
const ANCHOR_DEFAULT_MIN_SCORE: f64 = 0.3;

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct BboxOut {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayoutItemOut {
    pub id: String,
    pub stable_key: String,
    pub kind: String,
    pub section: String,
    pub page: u32,
    pub page_index: u32,
    pub bbox: BboxOut,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub layout_region_id: String,
    /// `#2.1` or `#2.1#p3`; only on section/paragraph anchors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// `#2.1`; the numbered heading this paragraph belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_anchor: Option<String>,
    /// 1-based body-block ordinal inside the owning section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paragraph_index: Option<u32>,
    /// Original block text for section/paragraph anchors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl From<&LayoutIndexItem> for LayoutItemOut {
    fn from(i: &LayoutIndexItem) -> Self {
        Self {
            id: i.id.clone(),
            stable_key: i.stable_key.clone(),
            kind: i.kind.clone(),
            section: i.section.clone(),
            page: i.page,
            page_index: i.page_index,
            bbox: BboxOut {
                x: i.bbox.x,
                y: i.bbox.y,
                w: i.bbox.w,
                h: i.bbox.h,
            },
            score: i.score,
            title: i.title.clone(),
            layout_region_id: i.layout_region_id.clone(),
            anchor: None,
            section_anchor: None,
            paragraph_index: None,
            text: None,
        }
    }
}

impl LayoutItemOut {
    fn from_anchor(a: &anchors::DocumentAnchor) -> Self {
        Self {
            id: a.id.clone(),
            stable_key: a.id.clone(),
            kind: a.kind.clone(),
            section: a.kind.clone(),
            page: a.page,
            page_index: a.page_index,
            bbox: BboxOut {
                x: a.bbox.x,
                y: a.bbox.y,
                w: a.bbox.w,
                h: a.bbox.h,
            },
            score: a.score,
            title: a.title.clone(),
            layout_region_id: a.id.clone(),
            anchor: a.anchor.clone(),
            section_anchor: a.section_anchor.clone(),
            paragraph_index: a.paragraph_index,
            text: a.text.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayoutCountsOut {
    pub total: usize,
    pub figure: usize,
    pub table: usize,
    pub algorithm: usize,
    pub formula: usize,
    pub section: usize,
    pub paragraph: usize,
}

impl LayoutCountsOut {
    fn from_items(items: &[LayoutItemOut]) -> Self {
        let mut counts = Self {
            total: items.len(),
            figure: 0,
            table: 0,
            algorithm: 0,
            formula: 0,
            section: 0,
            paragraph: 0,
        };
        for item in items {
            match item.section.as_str() {
                "figure" => counts.figure += 1,
                "table" => counts.table += 1,
                "algorithm" => counts.algorithm += 1,
                "formula" => counts.formula += 1,
                "section" => counts.section += 1,
                "paragraph" => counts.paragraph += 1,
                _ => {}
            }
        }
        counts
    }
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayoutListOut {
    pub paper_path: String,
    pub index_path: String,
    pub generated_at: String,
    pub min_score: f64,
    pub counts: LayoutCountsOut,
    pub items: Vec<LayoutItemOut>,
}

impl From<LayoutListResult> for LayoutListOut {
    fn from(r: LayoutListResult) -> Self {
        let items: Vec<LayoutItemOut> = r.items.iter().map(LayoutItemOut::from).collect();
        Self {
            paper_path: r.paper_path,
            index_path: r.index_path,
            generated_at: r.generated_at,
            min_score: r.min_score,
            counts: LayoutCountsOut::from_items(&items),
            items,
        }
    }
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayoutGetOut {
    pub paper_path: String,
    pub index_path: String,
    pub generated_at: String,
    pub item: LayoutItemOut,
}

impl From<LayoutGetResult> for LayoutGetOut {
    fn from(r: LayoutGetResult) -> Self {
        Self {
            paper_path: r.paper_path,
            index_path: r.index_path,
            generated_at: r.generated_at,
            item: LayoutItemOut::from(&r.item),
        }
    }
}

/// Normalize `layout_list` kind filters. Unlike the sidebar index this MCP
/// surface also accepts `section` / `paragraph`, which read raw `layout.json`.
fn normalize_kinds(kinds: &[String]) -> Result<Vec<String>, AppError> {
    let mut out = Vec::new();
    for kind in kinds {
        let kind = kind.trim().to_ascii_lowercase();
        if kind.is_empty() {
            continue;
        }
        if INDEX_KINDS.contains(&kind.as_str()) || ANCHOR_KINDS.contains(&kind.as_str()) {
            out.push(kind);
        } else {
            return Err(AppError::domain(
                "usage",
                format!(
                    "unknown kind '{kind}' (use figure|image|chart|table|algorithm|formula|section|paragraph)"
                ),
            ));
        }
    }
    Ok(out)
}

/// List sidebar regions (`layout-index.json`) and/or document anchors
/// (`source/layout.json`). Empty `kind` keeps the previous index-only listing;
/// `section`/`paragraph` do not require `layout-index.json` when requested
/// alone, so external agents can read anchors right after layout analysis.
pub fn list(
    vault: &Path,
    ref_: &str,
    kinds: &[String],
    min_score: Option<f64>,
) -> Result<LayoutListOut, AppError> {
    let paper = paper::resolve_paper(vault, ref_)?;
    let filters = normalize_kinds(kinds)?;
    let anchor_filters: Vec<&str> = filters
        .iter()
        .map(String::as_str)
        .filter(|f| ANCHOR_KINDS.contains(f))
        .collect();
    if anchor_filters.is_empty() {
        return Ok(LayoutListOut::from(layout_index::list_regions(
            vault,
            &paper.path,
            kinds,
            min_score,
        )?));
    }

    let index_filters: Vec<String> = filters
        .iter()
        .filter(|f| !ANCHOR_KINDS.contains(&f.as_str()))
        .cloned()
        .collect();
    let threshold = min_score.unwrap_or(ANCHOR_DEFAULT_MIN_SCORE);
    let mut items: Vec<LayoutItemOut> = Vec::new();
    let (mut index_path, mut generated_at, mut effective_min_score) = (
        format!("{}/source/{}", paper.path, layout_index::LAYOUT_RAW_FILE),
        String::new(),
        threshold,
    );
    if !index_filters.is_empty() {
        let index = layout_index::list_regions(vault, &paper.path, &index_filters, min_score)?;
        index_path = index.index_path.clone();
        generated_at = index.generated_at.clone();
        effective_min_score = index.min_score;
        items.extend(index.items.iter().map(LayoutItemOut::from));
    }
    // Enumeration happens before score/kind filtering so low-confidence blocks
    // still reserve their paragraph ordinal, matching upstream. Anchors stay
    // `None` for unnumbered/ambiguous sections; they are still listed so
    // callers can see the heading text and coordinates.
    let anchor_items = anchors::load(vault, &paper.path)?;
    items.extend(
        anchor_items
            .iter()
            .filter(|a| {
                a.score + f64::EPSILON >= threshold && anchor_filters.contains(&a.kind.as_str())
            })
            .map(LayoutItemOut::from_anchor),
    );
    items.sort_by(|a, b| {
        a.page_index
            .cmp(&b.page_index)
            .then_with(|| a.bbox.y.total_cmp(&b.bbox.y))
    });
    let counts = LayoutCountsOut::from_items(&items);
    Ok(LayoutListOut {
        paper_path: paper.path,
        index_path,
        generated_at,
        min_score: effective_min_score,
        counts,
        items,
    })
}

/// Get one sidebar region by id or one document anchor by exact fragment
/// (`#2.1` / `#2.1#p3`).
pub fn get(vault: &Path, ref_: &str, id: &str) -> Result<LayoutGetOut, AppError> {
    let paper = paper::resolve_paper(vault, ref_)?;
    let id = id.trim();
    if id.starts_with('#') {
        let item = anchors::resolve(vault, &paper.path, id)?;
        return Ok(LayoutGetOut {
            index_path: format!("{}/source/{}", paper.path, layout_index::LAYOUT_RAW_FILE),
            paper_path: paper.path,
            generated_at: String::new(),
            item: LayoutItemOut::from_anchor(&item),
        });
    }
    Ok(LayoutGetOut::from(layout_index::get_region(
        vault,
        &paper.path,
        id,
    )?))
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageRegionOut {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub bbox: BboxOut,
}

impl From<&agentero_core::features::pdf::layout_index::RawLayoutRegion> for PageRegionOut {
    fn from(r: &agentero_core::features::pdf::layout_index::RawLayoutRegion) -> Self {
        Self {
            id: r.id.clone(),
            kind: r.kind.clone(),
            title: r.title.clone(),
            text: r.text.clone(),
            bbox: BboxOut {
                x: r.bbox.x,
                y: r.bbox.y,
                w: r.bbox.w,
                h: r.bbox.h,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageReadOut {
    /// Resolved vault-relative paper folder.
    pub paper_path: String,
    /// 1-based physical page requested.
    pub page: u32,
    /// Total physical pages present in the raw layout (0 when none).
    pub page_count: u32,
    /// Number of regions on the requested page, in reading order.
    pub region_count: usize,
    /// Paragraph-level (kind contains `text`) regions on the page.
    pub text_regions: Vec<PageRegionOut>,
    /// All regions on the page, including headers / figures / tables.
    pub regions: Vec<PageRegionOut>,
}

/// Read every layout region on a 1-based physical page of a paper.
///
/// Requires `{paper}/source/layout.json` (schemaVersion 3, produced by the
/// desktop layout analysis). `page` is the 1-based page number shown in the
/// PDF viewer; it is mapped to the 0-based `pageIndex` stored in the layout.
pub fn read_page(vault: &Path, ref_: &str, page: u32) -> Result<PageReadOut, AppError> {
    use agentero_core::features::pdf::layout_index::{self, RawLayoutRegion};
    let paper = paper::resolve_paper(vault, ref_)?;
    if page == 0 {
        return Err(AppError::message(format!(
            "page must be 1-based, got {page} (first page is 1)"
        )));
    }
    let layout = layout_index::load_raw_layout(vault, &paper.path)?;
    let page_index = page - 1;
    let page_regions: Vec<&RawLayoutRegion> = layout_index::page_regions(&layout, page_index);
    let to_out = |r: &RawLayoutRegion| PageRegionOut::from(r);
    let regions: Vec<PageRegionOut> = page_regions.iter().copied().map(to_out).collect();
    // Paragraph-level regions: anything carrying body text that is not a
    // figure/table/formula/algorithm. layout.json tags text with kinds
    // like paragraph / header / title, so a plain contains("text") test
    // is too narrow; exclude the non-text kinds instead.
    const NON_TEXT_KINDS: [&str; 6] = ["figure", "image", "chart", "table", "formula", "algorithm"];
    let text_regions: Vec<PageRegionOut> = page_regions
        .iter()
        .filter(|r| {
            r.text.as_deref().is_some_and(|t| !t.trim().is_empty())
                && !NON_TEXT_KINDS
                    .iter()
                    .any(|k| r.kind.to_ascii_lowercase().contains(k))
        })
        .copied()
        .map(to_out)
        .collect();
    Ok(PageReadOut {
        paper_path: paper.path,
        page,
        page_count: layout_index::page_count(&layout),
        region_count: regions.len(),
        text_regions,
        regions,
    })
}
