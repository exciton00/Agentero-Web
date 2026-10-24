//! MinerU body-parse engine: run the shared cloud extract and read the
//! `full.md` markdown and its `images/` assets from the result zip.

use agentero_core::error::AppError;
use agentero_core::features::paper::analyze::layout_hosted::engine::HostedProviderCredentials;
use agentero_core::features::paper::analyze::layout_hosted::mineru::{
    read_mineru_markdown_bundle, run_mineru_extract,
};
use agentero_core::features::paper::analyze::parse::engines::{
    BodyParseCtx, BodyParseEngine, BodyParseOutcome,
};
use agentero_core::features::paper::analyze::parse::BodyParseAsset;
use async_trait::async_trait;

pub(crate) struct MineruBodyEngine;

#[async_trait]
impl BodyParseEngine for MineruBodyEngine {
    fn id(&self) -> &'static str {
        "mineru"
    }

    async fn parse(&self, ctx: &BodyParseCtx<'_>) -> Result<BodyParseOutcome, AppError> {
        let pdf_bytes =
            std::fs::read(ctx.pdf_path).map_err(|e| AppError::message(format!("read pdf: {e}")))?;
        let file_name = ctx
            .pdf_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("paper.pdf")
            .to_string();
        let credentials = HostedProviderCredentials {
            api_key: ctx.credentials.api_key.clone(),
            base_url: ctx.credentials.base_url.clone(),
            language: ctx.credentials.language.clone(),
            is_ocr: ctx.credentials.is_ocr,
            self_hosted: ctx.credentials.self_hosted,
        };
        let zip_bytes =
            run_mineru_extract(&credentials, pdf_bytes, &file_name, &|_, _, _| {}, &|| {
                ctx.is_cancelled()
            })
            .await?;
        let bundle = read_mineru_markdown_bundle(&zip_bytes)?;
        Ok(BodyParseOutcome {
            markdown: bundle.markdown,
            assets: bundle
                .assets
                .into_iter()
                .map(|asset| BodyParseAsset {
                    relative_path: asset.relative_path,
                    bytes: asset.bytes,
                })
                .collect(),
            body_source: "mineru".to_string(),
            body_quality: "high".to_string(),
        })
    }
}
