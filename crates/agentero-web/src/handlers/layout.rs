//! Remote PDF layout-analysis RPC (`layout_remote_analyze_pdf` /
//! `layout_remote_probe`).
//!
//! The engines are shared with the desktop Host
//! (`agentero_core::features::paper::analyze::layout_hosted`); this shell only
//! reads the provider credentials from the server-hosted settings and runs the
//! engine. MinerU/Paddle results stream `layout-remote:progress` through the
//! Web host's `AppHandle` → `WebHostHooks` → WebSocket bus, so the event name
//! and payload are byte-identical to the desktop.
//!
//! Privacy: the base URL / API key never live in the repository — they are
//! read at request time from `AppSettingsStore` (`layout.providerConfigs`).

use serde_json::Value;

use agentero_core::features::paper::analyze::layout_hosted::{
    engine_for, resolve_remote_credentials, HostedLayoutAnalyzer, LayoutAnalyzeContext,
    LayoutRemoteAnalyzePdfArgs, LayoutRemoteProbeArgs, DEFAULT_REMOTE_PROVIDER,
};

use crate::error::{fail, fail_msg, ok, parse_args, HandlerResult};
use crate::rpc::RpcCtx;

/// Count the pages of a PDF whose base64 payload is about to be uploaded.
///
/// PDFium (via `agentero_core::features::pdf::text::page_count`) is the exact
/// answer and only opens the document's page tree — no text/layout pass. When
/// PDFium cannot load the file (corrupt PDF, missing shared library) fall back
/// to counting `/Type /Page` tokens in the raw bytes so the guard still works
/// for ordinary uncompressed PDFs; a compressed object stream may hide pages,
/// which fails open (the settings default is "no limit" anyway).
fn count_pdf_pages(pdf: &[u8]) -> Option<u32> {
    match agentero_core::features::pdf::text::page_count(pdf) {
        Ok(pages) => Some(pages),
        Err(error) => {
            log::warn!(
                "layout_remote: PDFium page count failed ({error}); falling back to /Type /Page scan"
            );
            let pages = naive_page_count(pdf);
            (pages > 0).then_some(pages)
        }
    }
}

/// Count `/Type /Page` dictionaries in `pdf`, ignoring the `/Type /Pages` tree
/// nodes. Only used when PDFium rejects the document.
fn naive_page_count(pdf: &[u8]) -> u32 {
    const TYPE: &[u8] = b"/Type";
    const PAGE: &[u8] = b"/Page";
    let mut count = 0u32;
    let mut rest = pdf;
    while let Some(index) = find_bytes(rest, TYPE) {
        rest = &rest[index + TYPE.len()..];
        let after_type = rest.trim_ascii_start();
        if let Some(after_page) = after_type.strip_prefix(PAGE) {
            // `/Pages` starts with `/Page` too: require a token boundary.
            if after_page
                .first()
                .is_none_or(|byte| !byte.is_ascii_alphanumeric())
            {
                count += 1;
            }
        }
    }
    count
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Business error when a PDF has more pages than the configured cap.
fn layout_page_limit_error(pages: u32, max_pages: u32) -> Option<String> {
    (max_pages > 0 && pages > max_pages).then(|| {
        format!(
            "PDF has {pages} pages, above the layout analysis limit of {max_pages} (Settings → Layout)"
        )
    })
}

/// Server-side backstop for `layout.maxPages`: the SPA skips oversized PDFs
/// before building the request, but any other client still hits this guard.
fn guard_layout_page_limit(pdf_base64: &str, max_pages: u32) -> Result<(), String> {
    use base64::Engine as _;
    let pdf = base64::engine::general_purpose::STANDARD
        .decode(pdf_base64.trim())
        .map_err(|error| format!("layout_remote: invalid PDF base64: {error}"))?;
    let Some(pages) = count_pdf_pages(&pdf) else {
        return Ok(());
    };
    let Some(message) = layout_page_limit_error(pages, max_pages) else {
        return Ok(());
    };
    log::info!("layout_remote: rejected PDF with {pages} pages (limit {max_pages})");
    Err(message)
}

/// Resolve the provider id + engine (default provider is `paddle`, matching the
/// desktop command shell). Unknown ids are a business error.
fn provider_engine(
    provider: Option<String>,
) -> Result<std::sync::Arc<dyn HostedLayoutAnalyzer>, String> {
    let provider = provider.unwrap_or_else(|| DEFAULT_REMOTE_PROVIDER.to_string());
    engine_for(&provider).ok_or_else(|| format!("layout_remote: unknown provider: {provider}"))
}

/// `layout_remote_analyze_pdf({pdfBase64, fileName?, apiKey?, requestId?, provider?})`
/// → `ApiResult<LayoutRemoteAnalyzePdfResult>`.
pub async fn analyze_pdf(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let mut args: LayoutRemoteAnalyzePdfArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let engine = match provider_engine(args.provider.clone()) {
        Ok(engine) => engine,
        Err(error) => return fail_msg(error),
    };
    // Guard before credentials/upload: the WebView never needs to send a
    // multi-MB PDF that the configured page cap will reject anyway.
    let max_pages = ctx.state.settings.layout_max_pages();
    if max_pages > 0 {
        if let Err(error) = guard_layout_page_limit(&args.pdf_base64, max_pages) {
            return fail_msg(error);
        }
    }
    let credentials =
        resolve_remote_credentials(&ctx.state.settings, engine.id(), &mut args.api_key);
    let context = LayoutAnalyzeContext {
        app: ctx.state.host_handle(),
        credentials,
        args,
    };
    // The remote engine can block on an unresponsive self-hosted MinerU/Paddle
    // job; without a cap the browser's renderer job remains "running" forever.
    // Keep the default generous enough for forced OCR and allow an env override.
    let timeout_secs = std::env::var("AGENTERO_WEB_LAYOUT_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(300);
    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        engine.analyze_pdf(context),
    )
    .await
    {
        Ok(Ok(result)) => ok(result),
        Ok(Err(error)) => fail(error),
        Err(_) => fail_msg(format!(
            "layout_remote: analysis timed out after {timeout_secs}s; check the provider endpoint or disable force OCR"
        )),
    }
}

/// `layout_remote_probe({imageBase64, apiKey?, provider?})` →
/// `ApiResult<LayoutRemoteProbeResult>`.
pub async fn probe(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let mut args: LayoutRemoteProbeArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let engine = match provider_engine(args.provider.clone()) {
        Ok(engine) => engine,
        Err(error) => return fail_msg(error),
    };
    let credentials =
        resolve_remote_credentials(&ctx.state.settings, engine.id(), &mut args.api_key);
    match engine.probe(&credentials, args).await {
        Ok(result) => ok(result),
        Err(error) => fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    /// Minimal `pages`-page PDF with empty content streams (same construction
    /// as the core `pdf::text` tests). PDFium needs a real xref table.
    fn tiny_pdf(pages: usize) -> Vec<u8> {
        let kids: Vec<String> = (0..pages)
            .map(|index| format!("{} 0 R", index + 3))
            .collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                pages
            ),
        ];
        for index in 0..pages {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents {} 0 R >>",
                pages + 3 + index
            ));
        }
        for _ in 0..pages {
            objects.push("<< /Length 0 >>\nstream\nendstream".to_string());
        }
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", index + 1));
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

    #[test]
    fn guard_rejects_only_pdfs_above_the_limit() {
        let pdf = tiny_pdf(2);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&pdf);

        assert!(guard_layout_page_limit(&encoded, 2).is_ok());
        assert!(guard_layout_page_limit(&encoded, 3).is_ok());

        let error = guard_layout_page_limit(&encoded, 1).expect_err("page 2 > limit 1");
        assert!(error.contains("PDF has 2 pages"), "got: {error}");
        assert!(error.contains("limit of 1"), "got: {error}");
        assert!(error.contains("Settings → Layout"), "got: {error}");
    }

    #[test]
    fn guard_reports_the_limit_message_shape() {
        assert_eq!(layout_page_limit_error(2, 0), None, "0 = unlimited");
        assert_eq!(layout_page_limit_error(1, 2), None);
        assert_eq!(
            layout_page_limit_error(15, 10).as_deref(),
            Some("PDF has 15 pages, above the layout analysis limit of 10 (Settings → Layout)")
        );
    }

    #[test]
    fn guard_rejects_invalid_base64_before_upload() {
        let error = guard_layout_page_limit("not base64", 5).expect_err("decode error");
        assert!(error.contains("invalid PDF base64"), "got: {error}");
    }

    #[test]
    fn fallback_scanner_ignores_pages_tree_node() {
        let pdf = tiny_pdf(3);
        assert_eq!(naive_page_count(&pdf), 3);
    }
}
