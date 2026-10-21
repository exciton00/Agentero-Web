//! Tauri command shell for remote layout providers: resolves credentials
//! from settings and dispatches to the engine registry.

use crate::core::error::{map_err, ApiResult, AppError};
use crate::features::paper::analyze::layout::hosted::engine::{
    engine_for, HostedProviderCredentials, LayoutAnalyzeContext,
};
use crate::features::paper::analyze::layout::hosted::{
    resolve_remote_credentials, LayoutRemoteAnalyzePdfArgs, LayoutRemoteAnalyzePdfResult,
    LayoutRemoteProbeArgs, LayoutRemoteProbeResult, DEFAULT_REMOTE_PROVIDER,
};
use crate::features::system::settings::AppSettingsStore;
use tauri::{AppHandle, Manager};

/// Inject the stored provider access token before any `.await`
/// (managed state must not be held across await).
fn inject_provider_credentials(
    app: &AppHandle,
    provider: &str,
    api_key: &mut Option<String>,
) -> HostedProviderCredentials {
    let store = app.state::<AppSettingsStore>();
    resolve_remote_credentials(&store, provider, api_key)
}

fn unknown_provider(provider: &str) -> AppError {
    AppError::message(format!("layout_remote: unknown provider: {provider}"))
}

#[tauri::command]
#[specta::specta]
pub async fn layout_remote_analyze_pdf(
    app: AppHandle,
    mut args: LayoutRemoteAnalyzePdfArgs,
) -> ApiResult<LayoutRemoteAnalyzePdfResult> {
    use crate::core::log_util::OpTimer;

    let provider = args
        .provider
        .clone()
        .unwrap_or_else(|| DEFAULT_REMOTE_PROVIDER.to_string());
    let Some(engine) = engine_for(&provider) else {
        return map_err(unknown_provider(&provider));
    };

    // Host keeps the real access token; the WebView sends a `*`-mask or nothing.
    let credentials = inject_provider_credentials(&app, engine.id(), &mut args.api_key);

    let op = OpTimer::start_with(
        "layout_remote_analyze_pdf",
        format!("pdf_chars={}", args.pdf_base64.len()),
    );
    let ctx = LayoutAnalyzeContext {
        app: crate::features::host_hooks::wrap(&app),
        credentials,
        args,
    };
    match engine.analyze_pdf(ctx).await {
        Ok(r) => {
            op.finish_ok();
            ApiResult::ok(r)
        }
        Err(e) => {
            op.finish_err(&e);
            map_err(e)
        }
    }
}

#[tauri::command]
#[specta::specta]
pub async fn layout_remote_probe(
    app: AppHandle,
    mut args: LayoutRemoteProbeArgs,
) -> ApiResult<LayoutRemoteProbeResult> {
    use crate::core::log_util::OpTimer;

    let provider = args
        .provider
        .clone()
        .unwrap_or_else(|| DEFAULT_REMOTE_PROVIDER.to_string());
    let Some(engine) = engine_for(&provider) else {
        return map_err(unknown_provider(&provider));
    };

    let credentials = inject_provider_credentials(&app, engine.id(), &mut args.api_key);

    let op = OpTimer::start("layout_remote_probe");
    match engine.probe(&credentials, args).await {
        Ok(r) => {
            op.finish_ok();
            ApiResult::ok(r)
        }
        Err(e) => {
            op.finish_err(&e);
            map_err(e)
        }
    }
}
