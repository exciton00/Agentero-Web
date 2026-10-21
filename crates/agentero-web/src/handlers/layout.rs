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
    let credentials =
        resolve_remote_credentials(&ctx.state.settings, engine.id(), &mut args.api_key);
    let context = LayoutAnalyzeContext {
        app: ctx.state.host_handle(),
        credentials,
        args,
    };
    match engine.analyze_pdf(context).await {
        Ok(result) => ok(result),
        Err(error) => fail(error),
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
