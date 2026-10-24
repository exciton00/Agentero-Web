//! OpenAI-compatible VLM OCR engine (SiliconFlow preset): render pages to
//! PNG in the isolated PDFium worker, then convert each page to markdown via
//! `POST {base}/chat/completions` with an inline `image_url` data URL.

use agentero_core::error::AppError;
use agentero_core::http;
use agentero_core::features::paper::analyze::parse::engines::{
    BodyParseCtx, BodyParseEngine, BodyParseOutcome,
};
use agentero_core::features::paper::analyze::parse::{
    run_liteparse_render_pngs, RenderedPngPage, CANCELLED_MESSAGE, VLM_MAX_PAGES,
};
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) const DEFAULT_VLM_BASE_URL: &str = "https://api.siliconflow.cn/v1";
pub(crate) const DEFAULT_VLM_MODEL: &str = "PaddlePaddle/PaddleOCR-VL-1.5";
const PAGE_CONCURRENCY: usize = 3;
const PAGE_TIMEOUT: Duration = Duration::from_secs(90);
/// Give up when more than this share of pages still fails after one retry.
const MAX_FAILED_PAGE_RATIO: f64 = 0.3;

/// One registration of the VLM OCR path. The same engine serves several backend
/// ids, so it carries the one it was registered under: core's fallback note
/// quotes the engine id and must name the provider the user selected.
pub(crate) struct OpenAiVlmBodyEngine {
    id: &'static str,
}

impl OpenAiVlmBodyEngine {
    pub(crate) fn new(id: &'static str) -> Self {
        Self { id }
    }
}

/// Endpoint + model resolved from the provider credentials.
struct VlmTarget {
    base: String,
    api_key: String,
    model: String,
    prompt: String,
}

fn resolve_target(ctx: &BodyParseCtx<'_>) -> Result<VlmTarget, AppError> {
    let api_key = ctx
        .credentials
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            AppError::message("OpenAI-compatible OCR requires apiKey (Settings → Layout)")
        })?
        .to_string();
    let base = ctx
        .credentials
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_VLM_BASE_URL)
        .trim_end_matches('/')
        .to_string();
    let model = ctx
        .credentials
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or(DEFAULT_VLM_MODEL)
        .to_string();
    // An explicit prompt wins; otherwise fall back to the per-model default.
    let prompt = ctx
        .credentials
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| prompt_for_model(&model).to_string());
    Ok(VlmTarget {
        base,
        api_key,
        model,
        prompt,
    })
}

#[async_trait]
impl BodyParseEngine for OpenAiVlmBodyEngine {
    fn id(&self) -> &'static str {
        self.id
    }

    async fn parse(&self, ctx: &BodyParseCtx<'_>) -> Result<BodyParseOutcome, AppError> {
        let target = resolve_target(ctx)?;
        // Rendering runs in the killable PDFium worker; the guard keeps the
        // PNG directory alive until every page request finished.
        let (pages, guard) = run_liteparse_render_pngs(ctx.pdf_path, ctx.task_id).await?;
        let markdown = ocr_rendered_pages(&pages, guard.path(), ctx, &target).await?;
        Ok(BodyParseOutcome {
            markdown,
            assets: Vec::new(),
            body_source: "vlm".to_string(),
            body_quality: "medium".to_string(),
        })
    }
}

/// OCR every rendered page and join the per-page markdown in page order.
async fn ocr_rendered_pages(
    pages: &[RenderedPngPage],
    dir: &std::path::Path,
    ctx: &BodyParseCtx<'_>,
    target: &VlmTarget,
) -> Result<String, AppError> {
    if pages.is_empty() {
        return Err(AppError::message("PDF rendered no pages"));
    }
    let truncated = pages.len() >= VLM_MAX_PAGES;
    let client = http::client_with(PAGE_TIMEOUT, 10, http::USER_AGENT)?;

    let total = pages.len();
    let mut page_futures = Vec::with_capacity(total);
    for (index, page) in pages.iter().enumerate() {
        page_futures.push(process_page(index, page, dir, ctx, &client, target));
    }
    let results: Vec<(usize, Result<String, AppError>)> = stream::iter(page_futures)
        .buffer_unordered(PAGE_CONCURRENCY)
        .collect()
        .await;

    if ctx.is_cancelled() {
        return Err(AppError::message(CANCELLED_MESSAGE));
    }

    let mut page_texts: Vec<String> = vec![String::new(); total];
    let mut failed = 0usize;
    for (index, outcome) in results {
        match outcome {
            Ok(text) => page_texts[index] = clean_page_markdown(&text),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains(CANCELLED_MESSAGE) {
                    return Err(e);
                }
                log::warn!(target: "agentero::pdf_parse", "VLM OCR page {} failed: {msg}", index + 1);
                failed += 1;
                page_texts[index] = format!("<!-- page {}: OCR failed -->", index + 1);
            }
        }
    }
    if failed as f64 > total as f64 * MAX_FAILED_PAGE_RATIO {
        return Err(AppError::message(format!(
            "VLM OCR failed on {failed}/{total} pages"
        )));
    }

    let mut markdown = page_texts
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if truncated {
        markdown.push_str(&format!(
            "\n\n<!-- truncated to the first {} pages -->",
            VLM_MAX_PAGES
        ));
    }
    Ok(markdown)
}

async fn process_page(
    index: usize,
    page: &RenderedPngPage,
    dir: &std::path::Path,
    ctx: &BodyParseCtx<'_>,
    client: &reqwest::Client,
    target: &VlmTarget,
) -> (usize, Result<String, AppError>) {
    if page.is_solid_fill {
        return (index, Ok(String::new()));
    }
    if ctx.is_cancelled() {
        return (index, Err(AppError::message(CANCELLED_MESSAGE)));
    }
    let png = match std::fs::read(dir.join(&page.file)) {
        Ok(bytes) => bytes,
        Err(e) => return (index, Err(AppError::message(format!("read page png: {e}")))),
    };
    let mut outcome = ocr_page(client, target, &png).await;
    if outcome.is_err() && !ctx.is_cancelled() {
        outcome = ocr_page(client, target, &png).await;
    }
    (index, outcome)
}

/// Model-specific OCR prompt (SiliconFlow-hosted models need exact prompts).
fn prompt_for_model(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.contains("deepseek-ocr") {
        "<image>\n<|grounding|>Convert the document to markdown."
    } else if m.contains("paddleocr") {
        "OCR:"
    } else {
        "Convert this document page to markdown. Output markdown only."
    }
}

async fn ocr_page(
    client: &reqwest::Client,
    target: &VlmTarget,
    png_bytes: &[u8],
) -> Result<String, AppError> {
    let data_url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png_bytes)
    );
    let body = json!({
        "model": target.model,
        "temperature": 0,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "image_url", "image_url": { "url": data_url } },
                { "type": "text", "text": target.prompt.as_str() }
            ]
        }]
    });
    let response = client
        .post(format!("{}/chat/completions", target.base))
        .bearer_auth(&target.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::message(format!("VLM OCR request failed: {e}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| AppError::message(format!("VLM OCR response read failed: {e}")))?;
    if !status.is_success() {
        let snippet = http::http_err_snippet(&text);
        return Err(AppError::message(format!(
            "VLM OCR request failed (HTTP {status}): {snippet}"
        )));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| AppError::message(format!("Unexpected VLM OCR response: {e}")))?;
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::message("Unexpected VLM OCR response: missing content"))
}

/// Strip DeepSeek-OCR grounding annotations and unwrap a whole-page markdown
/// code fence.
///
/// Grounding output is `<|ref|>label<|/ref|><|det|>[[box]]<|/det|>\ncontent`:
/// the ref payload is the layout category (`text`, `title`, …), not prose, so
/// both spans are dropped whole — keeping them would litter the body with
/// stray "text" / "sub_title" lines.
fn clean_page_markdown(text: &str) -> String {
    let stripped = strip_tagged_span(text, "<|ref|>", "<|/ref|>");
    let stripped = strip_tagged_span(&stripped, "<|det|>", "<|/det|>");
    // PaddleOCR-VL emits `<|LOC_123|>` box tokens when driven by a prompt it
    // does not recognize; they are pure noise in a body document.
    let stripped = strip_tagged_span(&stripped, "<|LOC_", "|>");
    let trimmed = stripped.trim();
    let unfenced = trimmed
        .strip_prefix("```markdown")
        .or_else(|| trimmed.strip_prefix("```md"))
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|rest| rest.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    unfenced.to_string()
}

/// Remove every `open … close` span, including its payload. An unterminated
/// `open` drops only the marker so the trailing content survives.
fn strip_tagged_span(text: &str, open: &str, close: &str) -> String {
    let mut out = text.to_string();
    while let Some(start) = out.find(open) {
        let rest = &out[start + open.len()..];
        match rest.find(close) {
            Some(offset) => {
                let end = start + open.len() + offset + close.len();
                out.replace_range(start..end, "");
            }
            None => out.replace_range(start..start + open.len(), ""),
        }
    }
    out
}
