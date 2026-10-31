//! DOI enrichment for reference drafts.
//!
//! Crossref reference lists routinely carry entries with only a DOI (no title /
//! authors / year). When online lookup is enabled we resolve those DOIs through
//! the shared [`CrossrefApi`] work lookup so the References panel can display
//! real metadata, and so the resolved title can match the locally-parsed raw
//! citation list (giving that row its DOI + link too).
//!
//! The pass is deliberately bounded: only unresolved DOI-bearing drafts (at
//! most [`MAX_DOI_LOOKUPS`]), the scholar-api global concurrency limit applies,
//! and one hard deadline covers the whole pass. Any failure/timeout simply
//! leaves the draft untouched — offline parsing still provides the display.

use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};

use crate::features::paper::scholar_api::sources::crossref::CrossrefApi;
use crate::features::paper::scholar_api::traits::AcademicApi;
use crate::features::paper::scholar_api::{ApiError, ApiPaper, ApiQuery};

use super::RefDraft;

/// Upper bound on per-paper DOI lookups (bibliographies rarely exceed this;
/// keeps a hostile/broken Crossref list from stalling the parse).
pub const MAX_DOI_LOOKUPS: usize = 40;
/// Whole-pass deadline; individual requests already time out in the HTTP client.
const ENRICH_DEADLINE: Duration = Duration::from_secs(20);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnrichReport {
    /// DOI-bearing unresolved drafts considered.
    pub attempted: usize,
    /// Drafts that gained a title (or other metadata).
    pub resolved: usize,
    /// Lookups that failed / were cut off by the deadline.
    pub failed: usize,
}

/// Draft indexes that still need a DOI metadata lookup, in document order.
pub(crate) fn doi_lookup_targets(drafts: &[RefDraft]) -> Vec<(usize, String)> {
    drafts
        .iter()
        .enumerate()
        .filter(|(_, draft)| draft.title.is_none())
        .filter_map(|(index, draft)| {
            draft
                .doi
                .as_deref()
                .map(str::trim)
                .filter(|doi| !doi.is_empty())
                .map(|doi| (index, doi.to_string()))
        })
        .take(MAX_DOI_LOOKUPS)
        .collect()
}

/// Fill missing display metadata from one resolved Crossref work. Never
/// overwrites locally parsed values (Crossref is treated as supplementary here).
pub(crate) fn apply_lookup(draft: &mut RefDraft, paper: &ApiPaper) -> bool {
    let mut changed = false;
    if draft.title.is_none() {
        let title = paper.title.trim();
        if !title.is_empty() {
            draft.title = Some(title.to_string());
            changed = true;
        }
    }
    if draft.authors.is_empty() && !paper.authors.is_empty() {
        draft.authors = paper.authors.clone();
        changed = true;
    }
    if draft.year.is_none() {
        if let Some(year) = paper.year {
            draft.year = Some(year);
            changed = true;
        }
    }
    if draft.venue.is_none() {
        if let Some(venue) = paper
            .venue
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            draft.venue = Some(venue.to_string());
            changed = true;
        }
    }
    if draft.url.is_none() {
        if let Some(url) = paper
            .urls
            .landing
            .clone()
            .or_else(|| paper.urls.html.clone())
            .filter(|url| !url.trim().is_empty())
        {
            draft.url = Some(url);
            changed = true;
        }
    }
    if draft.arxiv_id.is_none() && paper.identifiers.arxiv_id.is_some() {
        draft.arxiv_id = paper.identifiers.arxiv_id.clone();
        changed = true;
    }
    changed
}

/// Resolve DOI-only drafts via Crossref. Best-effort: failures leave the draft
/// as it was and are reported only in the aggregate count.
pub async fn enrich_doi_only(drafts: &mut [RefDraft]) -> EnrichReport {
    let targets = doi_lookup_targets(drafts);
    if targets.is_empty() {
        return EnrichReport::default();
    }

    let mut pending = FuturesUnordered::new();
    for (index, doi) in targets.iter().cloned() {
        pending.push(async move {
            let api = CrossrefApi;
            let result = api.fetch(&ApiQuery::Doi(doi)).await;
            (index, result)
        });
    }

    let mut report = EnrichReport {
        attempted: targets.len(),
        ..Default::default()
    };
    let mut results: Vec<(usize, Result<Vec<ApiPaper>, ApiError>)> = Vec::new();
    let deadline = tokio::time::sleep(ENRICH_DEADLINE);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            next = pending.next() => match next {
                Some(result) => results.push(result),
                None => break,
            },
            _ = &mut deadline => {
                log::debug!(
                    target: "agentero::refs",
                    "DOI enrichment deadline hit after {} of {} lookups",
                    results.len(),
                    targets.len()
                );
                break;
            }
        }
    }

    for (index, result) in results {
        match result {
            Ok(papers) => {
                if let Some(paper) = papers.first() {
                    let Some(draft) = drafts.get_mut(index) else {
                        continue;
                    };
                    if apply_lookup(draft, paper) {
                        report.resolved += 1;
                    } else {
                        report.failed += 1;
                    }
                } else {
                    report.failed += 1;
                }
            }
            Err(error) => {
                log::debug!(
                    target: "agentero::refs",
                    "DOI lookup failed for draft {index}: {error}"
                );
                report.failed += 1;
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(title: Option<&str>, doi: Option<&str>) -> RefDraft {
        RefDraft {
            key: None,
            number: None,
            raw: None,
            title: title.map(str::to_string),
            authors: Vec::new(),
            year: None,
            venue: None,
            doi: doi.map(str::to_string),
            arxiv_id: None,
            url: None,
            source: "crossref",
        }
    }

    fn api_paper() -> ApiPaper {
        ApiPaper {
            identifiers: crate::features::paper::scholar_api::PaperIdentifiers {
                doi: Some("10.1109/TMTT.2002.807687".into()),
                arxiv_id: None,
                isbn: None,
                pmid: None,
            },
            title: "A resolved title".into(),
            authors: vec!["Y. F. Wu".into()],
            year: Some(2002),
            date: Some("2002".into()),
            venue: Some("IEEE Trans. Microw. Theory Tech.".into()),
            volume: None,
            issue: None,
            pages: None,
            publisher: None,
            abstract_text: None,
            urls: crate::features::paper::scholar_api::PaperUrls {
                pdf: None,
                html: Some("https://doi.org/10.1109/TMTT.2002.807687".into()),
                landing: Some("https://doi.org/10.1109/TMTT.2002.807687".into()),
            },
            citation_count: None,
            language: None,
            source: "crossref",
            raw: None,
        }
    }

    #[test]
    fn targets_only_unresolved_doi_drafts_and_are_capped() {
        let drafts = vec![
            draft(Some("already resolved"), Some("10.1/a")),
            draft(None, Some("10.1/b")),
            draft(None, None),
            draft(None, Some("   ")),
            draft(None, Some("10.1/c")),
        ];
        assert_eq!(
            doi_lookup_targets(&drafts),
            vec![(1, "10.1/b".to_string()), (4, "10.1/c".to_string())]
        );

        let many: Vec<RefDraft> = (0..MAX_DOI_LOOKUPS + 5)
            .map(|i| draft(None, Some(&format!("10.1/{i}"))))
            .collect();
        assert_eq!(doi_lookup_targets(&many).len(), MAX_DOI_LOOKUPS);
    }

    #[test]
    fn applies_missing_metadata_without_overwriting_local_values() {
        let paper = api_paper();
        let mut bare = draft(None, Some("10.1109/TMTT.2002.807687"));
        assert!(apply_lookup(&mut bare, &paper));
        assert_eq!(bare.title.as_deref(), Some("A resolved title"));
        assert_eq!(bare.authors, vec!["Y. F. Wu".to_string()]);
        assert_eq!(bare.year, Some(2002));
        assert_eq!(
            bare.venue.as_deref(),
            Some("IEEE Trans. Microw. Theory Tech.")
        );
        assert_eq!(
            bare.url.as_deref(),
            Some("https://doi.org/10.1109/TMTT.2002.807687")
        );

        let mut local = draft(Some("Local title"), Some("10.1109/TMTT.2002.807687"));
        assert!(apply_lookup(&mut local, &paper));
        assert_eq!(local.title.as_deref(), Some("Local title"));
        assert_eq!(local.year, Some(2002));
    }

    #[test]
    fn apply_lookup_reports_false_when_remote_has_no_usable_metadata() {
        let mut paper = api_paper();
        paper.title = "   ".into();
        paper.authors.clear();
        paper.year = None;
        paper.venue = None;
        paper.urls = crate::features::paper::scholar_api::PaperUrls::default();
        let mut bare = draft(None, Some("10.1/x"));
        assert!(!apply_lookup(&mut bare, &paper));
        assert!(bare.title.is_none());
    }
}
