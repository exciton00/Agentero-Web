//! Settings handlers: the shared XDG `settings.json` store.

use serde_json::Value;

use crate::error::{blocking, fail, fail_msg, ok, HandlerResult};
use crate::rpc::RpcCtx;

/// `settings_get` → `ApiResult<SettingsGetResult>`.
pub async fn get(ctx: &RpcCtx) -> HandlerResult {
    let store = ctx.state.settings.clone();
    let result = blocking(move || store.get().map_err(|error| error.to_string())).await;
    match result {
        Ok(snapshot) => ok(snapshot),
        Err(error) => fail_msg(error),
    }
}

/// `settings_set(settings)` → `ApiResult<AppSettings>`; broadcasts
/// `settings:changed` to every tab, mirroring the desktop Host.
pub async fn set(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let raw = args.get("settings").unwrap_or(args);
    let settings: agentero_core::features::system::settings::AppSettings =
        match serde_json::from_value(raw.clone()) {
            Ok(settings) => settings,
            Err(error) => return fail_msg(format!("invalid settings: {error}")),
        };

    if let Err(error) = agentero_core::http::configure_proxy(
        settings.network_proxy_enabled,
        &settings.network_proxy_url,
    ) {
        return fail(error);
    }
    if let Err(error) = agentero_core::http::configure_github_mirror(
        settings.github_mirror_enabled,
        &settings.github_mirror_base_url,
    ) {
        return fail(error);
    }

    let store = ctx.state.settings.clone();
    let result = blocking(move || store.set(settings).map_err(|error| error.to_string())).await;
    match result {
        Ok(settings) => {
            crate::parser_engines::refresh_from_store(&ctx.state.settings);
            ctx.state.events.emit("settings:changed", &settings);
            ok(settings)
        }
        Err(error) => fail_msg(error),
    }
}
