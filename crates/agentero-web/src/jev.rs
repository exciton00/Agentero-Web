//! TypeSafe jEV smart highlights — headless Web port.
//!
//! Domain logic (sentence extraction, question building, score decisions,
//! suggestion geometry) is ported from the desktop
//! `src-tauri/src/features/jev/service.rs`; every HTTP call routes through the
//! shared [`SystemOneProvider`] so credentials and transport error handling
//! have a single owner (Settings → Experimental → Decision).
//!
//! Execution model: `job_jev_smart_highlights_enqueue` registers a host-run
//! TaskCenter job ([`JobKind::JevSmartHighlights`]) that scores the PDF's
//! sentences in concurrent batches, writes `scoring` / `locating` progress, and
//! merges `{ result: { highlights } }` into the terminal job snapshot — the
//! renderer reads suggestions from the same `job:changed` event it already
//! listens to (desktop `JobCenter::merge_running_job_params` parity).
//!
//! `jev_probe_health` backs the Settings → Experimental "test connection"
//! button. Credentials are read lazily from the settings store at request time,
//! so edits take effect without rebuilding anything; the API key is never
//! logged or echoed in responses.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use agentero_core::decision::SystemOneProvider;
use agentero_core::error::AppError;
use agentero_core::features::paper::capabilities::probe_paper_caps;
use agentero_core::features::pdf::locate::{extract_text_in_pdf, ExtractedSentence, NormRect};
use agentero_core::features::system::settings::AppSettingsStore;
use futures_util::stream::{self, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::error::{fail, fail_msg, ok, parse_args, typed_err, typed_ok, HandlerResult};
use crate::rpc::RpcCtx;
use crate::tasks::{JobHandle, JobKind, JobLane, JobSnapshot, JobSpec, TaskCenter};

/// Desktop-compatible message when the Decision provider has no usable key.
/// `AppSettingsStore::decision_config` treats an empty key / UI mask as unset.
pub const NO_API_KEY_ERROR: &str = "decision provider API key is not configured";
/// Stable frontend key for "this PDF has no extractable sentences" (the viewer
/// localizes it; see `pdf.jevNoReadableText`).
const NO_READABLE_TEXT_ERROR: &str = "jevNoReadableText";
const CANCELLED_ERROR: &str = "jEV smart highlight cancelled";
const NO_PDF_ERROR: &str = "No local PDF for smart highlight";

const BATCH_SIZE: usize = 32;
const JEV_CONCURRENCY: usize = 16;
const MIN_SENTENCE_LEN: usize = 40;
const MAX_SENTENCE_LEN: usize = 250;
const SCORE_THRESHOLD: f64 = 2.0;

/// One smart-highlight suggestion returned by the job.
///
/// Wire shape matches the desktop `SuggestedHighlight`
/// (`src/lib/core/bindings.ts` / `SuggestedHighlight` in the PDF viewer):
/// camelCase fields, normalized page rects, and the page size used to convert
/// them back to PDF points on the renderer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedHighlight {
    pub quote: String,
    pub page: u32,
    pub rects: Vec<NormRect>,
    pub color: String,
    pub category: String,
    pub score: f64,
    pub page_width: f32,
    pub page_height: f32,
}

#[derive(Debug, Clone)]
struct Sentence {
    id: String,
    text: String,
    page: u32,
    rects: Vec<NormRect>,
    page_width: f32,
    page_height: f32,
}

#[derive(Debug, Clone)]
struct HighlightDecision {
    key: &'static str,
    color: &'static str,
    name: &'static str,
    score: f64,
}

/// Scoring dimensions → palette color + user-facing category name.
const DIMS: [(&str, &str, &str); 5] = [
    ("method", "blue", "核心方法"),
    ("claim", "purple", "关键判断"),
    ("novelty", "green", "反常识结论"),
    ("effect", "yellow", "核心效果"),
    ("limitation", "pink", "局限"),
];

/// Tie-break order when two dimensions score equally (desktop order).
const COLOR_PRIORITY: [&str; 5] = ["method", "novelty", "effect", "claim", "limitation"];

// ---------------------------------------------------------------------------
// Domain: sentence extraction + scoring
// ---------------------------------------------------------------------------

fn read_title_from_sidecar(paper_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(paper_dir.join("metadata.json")).ok()?;
    let json: Value = serde_json::from_str(&raw).ok()?;
    json.get("title")?
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Best-effort sentence extraction from a paper's PDF, including each
/// sentence's on-page geometry so a second locate pass is unnecessary.
fn extract_paper_sentences(pdf_path: &Path) -> Vec<Sentence> {
    let Ok(bytes) = std::fs::read(pdf_path) else {
        return Vec::new();
    };
    let Ok(extracted) = extract_text_in_pdf(&bytes) else {
        return Vec::new();
    };

    extracted
        .into_iter()
        .filter_map(clean_sentence)
        .enumerate()
        .map(
            |(idx, (text, page, rects, page_width, page_height))| Sentence {
                id: format!("s{idx}"),
                text,
                page,
                rects,
                page_width,
                page_height,
            },
        )
        .collect()
}

fn clean_sentence(s: ExtractedSentence) -> Option<(String, u32, Vec<NormRect>, f32, f32)> {
    let trimmed = s.text.trim();
    // Drop obvious Markdown/list/header noise.
    if trimmed.starts_with('#') || trimmed.starts_with('-') || trimmed.starts_with('*') {
        return None;
    }
    // Normalize internal whitespace for scoring while keeping the original
    // geometry for rendering.
    let cleaned = trimmed
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.len() < MIN_SENTENCE_LEN || cleaned.len() > MAX_SENTENCE_LEN {
        return None;
    }
    Some((cleaned, s.page, s.rects, s.page_width, s.page_height))
}

fn build_questions(sentences: &[Sentence]) -> HashMap<String, Value> {
    let mut questions = HashMap::new();
    for s in sentences {
        for (key, _, _) in DIMS {
            let qid = format!("{}_{}_score", s.id, key);
            let (instructions, criteria) = match key {
                "method" => (
                    format!("Does `sentences['{}']` describe a key method, algorithm, or technical contribution of the paper?", s.id),
                    vec!["Not a method / only background", "Mentions a method but not central", "Key method or algorithm", "Novel core method that defines the paper"],
                ),
                "claim" => (
                    format!("Does `sentences['{}']` contain a key claim, judgment, or central argument of the paper?", s.id),
                    vec!["No claim / purely factual background", "Minor claim", "Key claim or judgment", "Central thesis of the paper"],
                ),
                "novelty" => (
                    format!("Does `sentences['{}']` state a conclusion or finding that differs from common practice or prior work?", s.id),
                    vec!["Aligns with common knowledge", "Slight twist", "Notably different from prior assumptions", "Directly contradicts or overturns conventional wisdom"],
                ),
                "effect" => (
                    format!("Does `sentences['{}']` present a core experimental result, effect, or main finding?", s.id),
                    vec!["No result / background", "Secondary result", "Important result or effect", "Main result that demonstrates the paper's value"],
                ),
                _ => (
                    format!("Does `sentences['{}']` discuss limitations, caveats, failure cases, or future work?", s.id),
                    vec!["No limitation mentioned", "Minor caveat", "Important limitation", "Critical limitation that affects interpretation"],
                ),
            };
            questions.insert(
                qid,
                json!({
                    "type": "score",
                    "instructions": instructions,
                    "criteria": criteria,
                }),
            );
        }
    }
    questions
}

fn decide_highlight(scores: &HashMap<String, f64>) -> Option<HighlightDecision> {
    let mut candidates = Vec::new();
    for (key, color, name) in DIMS {
        if let Some(&score) = scores.get(key) {
            if score >= SCORE_THRESHOLD {
                candidates.push(HighlightDecision {
                    key,
                    color,
                    name,
                    score,
                });
            }
        }
    }
    if candidates.is_empty() {
        return None;
    }
    let priority_index: HashMap<&str, usize> = COLOR_PRIORITY
        .iter()
        .enumerate()
        .map(|(i, k)| (*k, i))
        .collect();
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| priority_index[a.key].cmp(&priority_index[b.key]))
    });
    Some(candidates[0].clone())
}

/// Score pre-extracted sentences and build highlights.
///
/// Split from [`jev_suggest_highlights_with_progress`] so tests can feed
/// synthetic sentences / a mock provider without a real PDF.
async fn suggest_highlights_from_sentences<F: FnMut(usize, usize) + Send>(
    provider: Arc<SystemOneProvider>,
    sentences: Vec<Sentence>,
    title: &str,
    cancel_token: &CancellationToken,
    on_progress: F,
) -> Result<Vec<SuggestedHighlight>, AppError> {
    let total_batches = sentences.chunks(BATCH_SIZE).len();
    let on_progress = Arc::new(Mutex::new(on_progress));
    let completed_batches = Arc::new(AtomicUsize::new(0));

    // Phase 1: score all sentences in concurrent batches.
    let batches: Vec<(usize, Vec<Sentence>)> = sentences
        .chunks(BATCH_SIZE)
        .enumerate()
        .map(|(idx, chunk)| (idx, chunk.to_vec()))
        .collect();

    let scored_batches: Vec<(usize, Vec<(Sentence, HighlightDecision)>)> = stream::iter(batches)
        .map(|(batch_index, chunk)| {
            let title = title.to_string();
            let on_progress = on_progress.clone();
            let completed_batches = completed_batches.clone();
            let provider = provider.clone();
            async move {
                if cancel_token.is_cancelled() {
                    return Err(AppError::message(CANCELLED_ERROR));
                }

                let state_items: HashMap<String, String> = chunk
                    .iter()
                    .map(|s| (s.id.clone(), s.text.clone()))
                    .collect();
                let questions = build_questions(&chunk);
                let request = json!({
                    "state": {
                        "paper_title": title,
                        "sentences": state_items,
                    },
                    "questions": questions,
                });

                let resp = provider.complete(request).await?;
                let answers = resp
                    .get("answers")
                    .and_then(|a| a.as_object())
                    .cloned()
                    .unwrap_or_default();

                let mut decisions = Vec::new();
                for s in chunk {
                    let mut scores = HashMap::new();
                    for (key, _, _) in DIMS {
                        let qid = format!("{}_{}_score", s.id, key);
                        let score = answers
                            .get(&qid)
                            .and_then(|a| a.get("score"))
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        scores.insert(key.to_string(), score);
                    }

                    if let Some(decision) = decide_highlight(&scores) {
                        decisions.push((s, decision));
                    }
                }

                let done = completed_batches.fetch_add(1, Ordering::SeqCst) + 1;
                if let Ok(mut cb) = on_progress.lock() {
                    cb(done, total_batches);
                }

                Ok::<_, AppError>((batch_index, decisions))
            }
        })
        .buffer_unordered(JEV_CONCURRENCY)
        .try_collect()
        .await?;

    // Sort by batch index so output order is deterministic.
    let mut scored_batches = scored_batches;
    scored_batches.sort_by_key(|(idx, _)| *idx);

    let scored_sentences: Vec<(Sentence, HighlightDecision)> = scored_batches
        .into_iter()
        .flat_map(|(_, decisions)| decisions)
        .collect();

    // Phase 2: build highlights from the pre-computed sentence geometry. No
    // second PDF locate pass is needed: `extract_text_in_pdf` already returned
    // each sentence's on-page rects.
    let mut all_highlights: Vec<SuggestedHighlight> = Vec::new();
    for (sentence, decision) in &scored_sentences {
        if cancel_token.is_cancelled() {
            return Err(AppError::message(CANCELLED_ERROR));
        }
        if sentence.rects.is_empty() {
            continue;
        }
        all_highlights.push(SuggestedHighlight {
            quote: sentence.text.clone(),
            page: sentence.page,
            rects: sentence.rects.clone(),
            color: decision.color.to_string(),
            category: decision.name.to_string(),
            score: decision.score,
            page_width: sentence.page_width,
            page_height: sentence.page_height,
        });
    }

    // Deduplicate by quote to avoid overlapping identical suggestions.
    let mut seen = HashSet::new();
    all_highlights.retain(|h| seen.insert(h.quote.clone()));

    Ok(all_highlights)
}

/// Extract highlights for one paper by calling jEV, with cancellation and
/// per-batch progress.
async fn jev_suggest_highlights_with_progress<F: FnMut(usize, usize) + Send>(
    provider: &Arc<SystemOneProvider>,
    pdf_path: &Path,
    title: &str,
    cancel_token: &CancellationToken,
    on_progress: F,
) -> Result<Vec<SuggestedHighlight>, AppError> {
    let sentences = extract_paper_sentences(pdf_path);
    if sentences.is_empty() {
        return Err(AppError::message(NO_READABLE_TEXT_ERROR));
    }
    suggest_highlights_from_sentences(
        provider.clone(),
        sentences,
        title,
        cancel_token,
        on_progress,
    )
    .await
}

/// Lightweight health probe: send one tiny `score` question to verify key and
/// endpoint (Settings → Experimental → Decision "test connection").
pub async fn jev_probe_health(provider: &SystemOneProvider) -> Result<(), AppError> {
    let request = json!({
        "state": { "paper_title": "probe" },
        "questions": {
            "health_score": {
                "type": "score",
                "instructions": "Sanity check: does the state contain a paper_title?",
                "criteria": ["No", "Maybe", "Yes"],
            }
        }
    });
    provider.complete(request).await.map(|_| ())
}

// ---------------------------------------------------------------------------
// Web host: provider assembly + job runner + RPC handlers
// ---------------------------------------------------------------------------

/// Build the shared provider over the settings store.
///
/// Credentials are resolved lazily per request, exactly like the desktop
/// `features::decision::build_provider`, so changing the key / base URL in
/// Settings applies to later calls without restarting the host.
fn build_provider(settings: &Arc<AppSettingsStore>) -> Arc<SystemOneProvider> {
    let settings = settings.clone();
    Arc::new(SystemOneProvider::with_shared_client(move || {
        settings
            .decision_config()
            .ok_or_else(|| AppError::message(NO_API_KEY_ERROR))
    }))
}

/// Register the smart-highlight job for one paper.
///
/// Arguments are the already-parsed RPC fields; kept separate from the handler
/// so tests can drive the full enqueue → runner → result-params path against a
/// mock System One endpoint.
fn enqueue_smart_highlights_job(
    tasks: &TaskCenter,
    settings: &Arc<AppSettingsStore>,
    vault_path: &str,
    paper_path: Option<&str>,
    lane: Option<JobLane>,
    force: bool,
) -> Result<JobSnapshot, String> {
    // Fail fast with a readable error before registering a doomed job. The
    // provider check repeats lazily in case settings are cleared mid-job.
    if settings.decision_config().is_none() {
        return Err(NO_API_KEY_ERROR.to_string());
    }
    let paper = paper_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| "jev smart highlights requires a paper path".to_string())?;
    let paper = agentero_core::fs::sanitize_vault_rel(&paper)?;

    let spec = JobSpec::new(JobKind::JevSmartHighlights, vault_path)?
        .paper_path(paper)
        .lane(lane)
        .force(force);
    let provider = build_provider(settings);
    let vault = spec.vault_path.clone();
    let paper = spec.paper_path.clone();
    Ok(tasks.spawn(spec, move |handle| {
        Box::pin(run_smart_highlights_job(handle, provider, vault, paper))
    }))
}

async fn run_smart_highlights_job(
    handle: JobHandle,
    provider: Arc<SystemOneProvider>,
    vault_path: String,
    paper_path: Option<String>,
) -> Result<(), String> {
    handle.ensure_running()?;
    let vault = agentero_core::fs::resolve_vault(&vault_path).map_err(|e| e.to_string())?;
    let paper =
        paper_path.ok_or_else(|| "jev smart highlights requires a paper path".to_string())?;
    let (paper_dir, _rel) =
        agentero_core::fs::resolve_paper_dir(&vault, &paper).map_err(|e| e.to_string())?;

    let caps = probe_paper_caps(&paper_dir);
    let pdf_path = caps.pdf_path.ok_or_else(|| NO_PDF_ERROR.to_string())?;
    let title = read_title_from_sidecar(&paper_dir)
        .or_else(|| {
            paper_dir
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "Untitled paper".to_string());

    handle.set_progress(Some(0.0), Some("scoring".to_string()));
    let progress_handle = handle.clone();
    let cancel_token = handle.cancel_token();
    let on_progress = move |current: usize, total: usize| {
        let progress = if total == 0 {
            0.0
        } else {
            (current as f32 / total as f32) * 100.0
        };
        // Desktop parity: the last batch flips to the instant locate phase.
        let phase = if current < total {
            "scoring"
        } else {
            "locating"
        };
        progress_handle.set_progress(Some(progress), Some(phase.to_string()));
    };

    let highlights = jev_suggest_highlights_with_progress(
        &provider,
        &pdf_path,
        &title,
        &cancel_token,
        on_progress,
    )
    .await
    .map_err(|error| error.to_string())?;

    // A cancel that landed between the last batch and the merge must not
    // publish a succeeded job with suggestions.
    handle.ensure_running()?;
    handle.merge_params(json!({ "result": { "highlights": highlights } }));
    log::info!(
        "jev smart highlights finished paper={} suggestions={}",
        paper,
        highlights.len()
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JevEnqueueArgs {
    vault_path: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    lane: Option<JobLane>,
    #[serde(default)]
    force: bool,
}

/// `job_jev_smart_highlights_enqueue({ vaultPath, path })` → `ApiResult<JobSnapshot>`.
pub async fn enqueue_smart_highlights(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: JevEnqueueArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    match enqueue_smart_highlights_job(
        &ctx.state.tasks,
        &ctx.state.settings,
        &args.vault_path,
        args.path.as_deref(),
        args.lane,
        args.force,
    ) {
        Ok(snapshot) => typed_ok(snapshot),
        Err(message) => fail_msg(message),
    }
}

/// `jev_probe_health()` → `ApiResult<null>`; verifies key + endpoint only.
pub async fn probe_health(ctx: &RpcCtx, _args: &Value) -> HandlerResult {
    if ctx.state.settings.decision_config().is_none() {
        return fail_msg(NO_API_KEY_ERROR);
    }
    let provider = build_provider(&ctx.state.settings);
    match jev_probe_health(&provider).await {
        Ok(()) => ok(()),
        Err(error) => fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::JobState;
    use agentero_core::features::system::settings::{AppSettings, DecisionSettings};
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::time::Duration;
    use tempfile::tempdir;

    // ---- fixtures -------------------------------------------------------

    fn sentence(id: &str, text: &str) -> Sentence {
        Sentence {
            id: id.to_string(),
            text: text.to_string(),
            page: 1,
            rects: vec![NormRect {
                x: 0.1,
                y: 0.2,
                w: 0.5,
                h: 0.03,
            }],
            page_width: 612.0,
            page_height: 792.0,
        }
    }

    fn extracted(text: &str) -> ExtractedSentence {
        ExtractedSentence {
            page: 1,
            text: text.to_string(),
            rects: vec![NormRect {
                x: 0.1,
                y: 0.2,
                w: 0.5,
                h: 0.03,
            }],
            page_width: 612.0,
            page_height: 792.0,
        }
    }

    /// Minimal one-page PDF with a real xref table (same trick as the core
    /// `pdf::locate` tests) so PDFium loads it without reconstruction.
    fn tiny_pdf(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 12 Tf 20 60 Td ({text}) Tj ET\n");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R \
              /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}endstream",
                content.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
        }
        let xref_at = out.len();
        out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for offset in &offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    /// Temp vault with one paper folder holding a text PDF + metadata sidecar.
    fn paper_vault(pdf_text: &str) -> (tempfile::TempDir, String, String) {
        let dir = tempdir().expect("temp vault");
        let vault = dir.path().to_string_lossy().to_string();
        let paper_dir = dir.path().join("papers").join("paper-1");
        std::fs::create_dir_all(&paper_dir).expect("paper dir");
        std::fs::write(paper_dir.join("paper.pdf"), tiny_pdf(pdf_text)).expect("pdf");
        std::fs::write(
            paper_dir.join("metadata.json"),
            r#"{ "title": "A Tiny Test Paper" }"#,
        )
        .expect("metadata");
        (dir, vault, "papers/paper-1".to_string())
    }

    // ---- mock System One endpoint ---------------------------------------

    #[derive(Clone)]
    struct MockScores(Arc<HashMap<String, f64>>);

    async fn mock_system_one(
        State(scores): State<MockScores>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut answers = serde_json::Map::new();
        if let Some(questions) = body.get("questions").and_then(Value::as_object) {
            for qid in questions.keys() {
                let score = DIMS
                    .iter()
                    .find(|(dim, _, _)| qid.ends_with(&format!("_{dim}_score")))
                    .and_then(|(dim, _, _)| scores.0.get(*dim).copied())
                    .unwrap_or(0.0);
                answers.insert(qid.clone(), json!({ "score": score }));
            }
        }
        Json(json!({ "answers": answers }))
    }

    /// Start a mock endpoint; returns its base URL (trailing slash).
    async fn mock_endpoint(scores: &[(&str, f64)]) -> String {
        let map: HashMap<String, f64> = scores
            .iter()
            .map(|(key, value)| ((*key).to_string(), *value))
            .collect();
        let app = Router::new()
            .route("/", post(mock_system_one))
            .with_state(MockScores(Arc::new(map)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/")
    }

    fn settings_with_endpoint(base_url: &str) -> Arc<AppSettingsStore> {
        let settings = AppSettings {
            decision: DecisionSettings {
                provider: "custom".to_string(),
                api_key: "test-key".to_string(),
                base_url: base_url.to_string(),
                model: "test-model".to_string(),
                smart_highlight: true,
            },
            ..Default::default()
        };
        Arc::new(AppSettingsStore::for_tests(settings))
    }

    fn provider_for(base_url: &str) -> Arc<SystemOneProvider> {
        let base_url = base_url.to_string();
        let auth = move || {
            Ok(agentero_core::decision::SystemOneCredentials {
                api_key: "test-key".to_string(),
                base_url: base_url.clone(),
                model: String::new(),
            })
        };
        Arc::new(SystemOneProvider::new(reqwest::Client::new(), auth))
    }

    // ---- domain unit tests ----------------------------------------------

    #[test]
    fn clean_sentence_filters_noise_and_lengths() {
        assert!(clean_sentence(extracted(
            "# heading with a very long title that is long enough"
        ))
        .is_none());
        assert!(clean_sentence(extracted(
            "- a list item long enough to clear the minimum length"
        ))
        .is_none());
        assert!(clean_sentence(extracted("too short")).is_none());
        let long = "x".repeat(MAX_SENTENCE_LEN + 1);
        assert!(clean_sentence(extracted(&long)).is_none());

        let cleaned = clean_sentence(extracted(
            "  This is a   reasonably long sentence\nwith wrapped lines that keep enough characters.  ",
        ))
        .expect("kept");
        assert_eq!(
            cleaned.0,
            "This is a reasonably long sentence with wrapped lines that keep enough characters."
        );
        assert_eq!(cleaned.1, 1);
        assert_eq!(cleaned.3, 612.0);
        assert_eq!(cleaned.4, 792.0);
    }

    #[test]
    fn build_questions_covers_every_dimension() {
        let questions = build_questions(&[sentence("s0", "text")]);
        assert_eq!(questions.len(), DIMS.len());
        for (key, _, _) in DIMS {
            let question = questions
                .get(&format!("s0_{key}_score"))
                .unwrap_or_else(|| panic!("missing {key} question"));
            assert_eq!(question["type"], "score");
            assert!(question["instructions"]
                .as_str()
                .unwrap_or_default()
                .contains("sentences['s0']"));
            assert_eq!(question["criteria"].as_array().map(Vec::len), Some(4));
        }
    }

    #[test]
    fn decide_highlight_uses_threshold_priority_and_top_score() {
        let mut scores = HashMap::new();
        scores.insert("method".to_string(), 1.0);
        assert!(decide_highlight(&scores).is_none());

        // Highest score wins regardless of priority order.
        scores.insert("method".to_string(), 2.0);
        scores.insert("effect".to_string(), 3.0);
        let decision = decide_highlight(&scores).expect("decision");
        assert_eq!(decision.key, "effect");
        assert_eq!(decision.color, "yellow");
        assert_eq!(decision.score, 3.0);

        // Equal scores fall back to the desktop priority order.
        let mut tied = HashMap::new();
        tied.insert("effect".to_string(), 2.5);
        tied.insert("novelty".to_string(), 2.5);
        let decision = decide_highlight(&tied).expect("tied decision");
        assert_eq!(decision.key, "novelty");
        assert_eq!(decision.color, "green");
    }

    // ---- scoring against a mock endpoint --------------------------------

    #[tokio::test]
    async fn suggestions_carry_geometry_and_are_ordered_by_batch() {
        let url = mock_endpoint(&[("method", 3.0), ("claim", 1.0)]).await;
        let provider = provider_for(&url);
        let cancel = CancellationToken::new();
        let mut progress = Vec::new();
        let highlights = suggest_highlights_from_sentences(
            provider,
            vec![
                sentence("s0", "first sentence that is definitely long enough"),
                sentence("s1", "second sentence that is definitely long enough"),
            ],
            "Tiny paper",
            &cancel,
            |done, total| progress.push((done, total)),
        )
        .await
        .expect("suggestions");

        assert_eq!(highlights.len(), 2);
        assert_eq!(
            highlights[0].quote,
            "first sentence that is definitely long enough"
        );
        assert_eq!(
            highlights[1].quote,
            "second sentence that is definitely long enough"
        );
        assert_eq!(highlights[0].color, "blue");
        assert_eq!(highlights[0].category, "核心方法");
        assert_eq!(highlights[0].score, 3.0);
        assert_eq!(highlights[0].page, 1);
        assert_eq!(highlights[0].rects.len(), 1);
        assert_eq!(highlights[0].page_width, 612.0);
        assert_eq!(highlights[0].page_height, 792.0);
        // The wire shape the viewer reads: pageWidth/pageHeight camelCase.
        let wire = serde_json::to_value(&highlights[0]).expect("serialize");
        assert!(wire.get("pageWidth").is_some());
        assert!(wire.get("pageHeight").is_some());
        assert_eq!(progress, vec![(1, 1)]);
    }

    #[tokio::test]
    async fn suggestions_dedupe_identical_quotes() {
        let url = mock_endpoint(&[("effect", 3.0)]).await;
        let provider = provider_for(&url);
        let cancel = CancellationToken::new();
        let text = "the same sentence appears twice and is long enough";
        let highlights = suggest_highlights_from_sentences(
            provider,
            vec![sentence("s0", text), sentence("s1", text)],
            "Tiny paper",
            &cancel,
            |_, _| {},
        )
        .await
        .expect("suggestions");
        assert_eq!(highlights.len(), 1);
    }

    #[tokio::test]
    async fn pre_cancelled_scoring_stops_before_any_request() {
        let url = mock_endpoint(&[("method", 3.0)]).await;
        let provider = provider_for(&url);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = suggest_highlights_from_sentences(
            provider,
            vec![sentence("s0", "sentence long enough to be scored normally")],
            "Tiny paper",
            &cancel,
            |_, _| {},
        )
        .await
        .expect_err("cancelled");
        assert!(error.to_string().contains("cancelled"), "got {error}");
    }

    #[tokio::test]
    async fn probe_health_succeeds_against_mock_endpoint() {
        let url = mock_endpoint(&[]).await;
        jev_probe_health(&provider_for(&url))
            .await
            .expect("probe ok");
    }

    #[tokio::test]
    async fn probe_health_surfaces_missing_key_error() {
        let auth = || Err(AppError::message(NO_API_KEY_ERROR));
        let provider = SystemOneProvider::new(reqwest::Client::new(), auth);
        let error = jev_probe_health(&provider).await.expect_err("no key");
        assert!(error.to_string().contains(NO_API_KEY_ERROR));
    }

    // ---- enqueue → job → result params ----------------------------------

    fn drain(
        rx: &mut tokio::sync::broadcast::Receiver<crate::events::EventFrame>,
    ) -> Vec<crate::events::EventFrame> {
        let mut frames = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            frames.push(frame);
        }
        frames
    }

    async fn wait_for_job_events(
        rx: &mut tokio::sync::broadcast::Receiver<crate::events::EventFrame>,
        job_id: &str,
        terminal: bool,
    ) -> Vec<crate::events::EventFrame> {
        let mut seen = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                for frame in drain(rx) {
                    if frame.event.starts_with("job:") {
                        seen.push(frame.clone());
                    }
                    let job = &frame.payload["job"];
                    let is_this = job["id"].as_str() == Some(job_id);
                    let state = job["state"].as_str().unwrap_or_default();
                    if is_this && terminal && matches!(state, "succeeded" | "failed" | "cancelled")
                    {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("job events");
        seen
    }

    #[tokio::test]
    async fn enqueue_runs_job_and_publishes_result_params() {
        let (vault_dir, vault, paper) = paper_vault(
            "This is a sufficiently long sentence about a trapping method in GaN HEMTs.",
        );
        let _ = &vault_dir;
        let url = mock_endpoint(&[("method", 3.0), ("effect", 2.0)]).await;
        let settings = settings_with_endpoint(&url);
        let bus = crate::events::EventBus::new();
        let mut rx = bus.subscribe();
        let tasks = TaskCenter::new(bus);

        let snapshot =
            enqueue_smart_highlights_job(&tasks, &settings, &vault, Some(&paper), None, false)
                .expect("enqueue");
        assert_eq!(snapshot.kind, JobKind::JevSmartHighlights);
        assert_eq!(snapshot.paper_path.as_deref(), Some(paper.as_str()));

        let events = wait_for_job_events(&mut rx, &snapshot.id, true).await;
        let final_job = events
            .iter()
            .filter_map(|frame| {
                let job = &frame.payload["job"];
                (job["id"].as_str() == Some(snapshot.id.as_str())).then(|| job.clone())
            })
            .next_back()
            .expect("terminal job snapshot");
        assert_eq!(final_job["state"], "succeeded");
        let highlights = &final_job["params"]["result"]["highlights"];
        let highlights = highlights.as_array().expect("highlights array");
        assert!(!highlights.is_empty());
        let first = &highlights[0];
        assert!(first["quote"].as_str().unwrap_or_default().len() > 30);
        assert_eq!(first["page"], 1);
        assert!(first["pageWidth"].as_f64().unwrap_or(0.0) > 0.0);
        assert!(["blue", "purple", "green", "yellow", "pink"]
            .contains(&first["color"].as_str().unwrap_or_default()));
        // Progress phases were reported before completion.
        let phases: Vec<&str> = events
            .iter()
            .filter_map(|frame| frame.payload["job"]["phase"].as_str())
            .collect();
        assert!(phases.contains(&"scoring"), "phases: {phases:?}");
    }

    #[tokio::test]
    async fn enqueue_without_key_returns_readable_error_without_job() {
        let (_dir, vault, paper) = paper_vault(
            "This is a sufficiently long sentence about a trapping method in GaN HEMTs.",
        );
        let settings = Arc::new(AppSettingsStore::for_tests(AppSettings::default()));
        let tasks = TaskCenter::new(crate::events::EventBus::new());
        let error =
            enqueue_smart_highlights_job(&tasks, &settings, &vault, Some(&paper), None, false)
                .expect_err("missing key");
        assert_eq!(error, NO_API_KEY_ERROR);
        assert!(tasks.list(None, None).is_empty(), "no job registered");
    }

    #[tokio::test]
    async fn cancel_settles_the_smart_highlight_job() {
        let (_dir, vault, paper) = paper_vault(
            "This is a sufficiently long sentence about a trapping method in GaN HEMTs.",
        );
        // Slow endpoint: the job is still scoring when we cancel.
        let app = Router::new().route(
            "/",
            post(|Json(body): Json<Value>| async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let questions = body
                    .get("questions")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let answers: serde_json::Map<String, Value> = questions
                    .keys()
                    .map(|qid| (qid.clone(), json!({ "score": 3.0 })))
                    .collect();
                Json(json!({ "answers": answers }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let settings = settings_with_endpoint(&format!("http://{addr}/"));
        let tasks = TaskCenter::new(crate::events::EventBus::new());
        let snapshot =
            enqueue_smart_highlights_job(&tasks, &settings, &vault, Some(&paper), None, false)
                .expect("enqueue");

        // Wait for the runner to start (phase scoring), then cancel.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = tasks.snapshot(&snapshot.id).map(|job| job.state);
                if matches!(state, Some(JobState::Running)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("job running");

        assert!(tasks.cancel(&snapshot.id));
        assert_eq!(
            tasks.snapshot(&snapshot.id).map(|job| job.state),
            Some(JobState::Cancelled)
        );
        // The runner must not resurrect the job after the token fires.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            tasks.snapshot(&snapshot.id).map(|job| job.state),
            Some(JobState::Cancelled)
        );
    }
}
