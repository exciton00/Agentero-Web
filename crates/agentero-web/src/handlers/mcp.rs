//! MCP server control RPCs for the headless Web host.
//!
//! Mirrors the desktop command shell (`src-tauri/src/integration/mcp/commands.rs`)
//! but owns the settings side effects the Web UI expects: `mcp_set_enabled` /
//! `mcp_set_port` persist `mcpEnabled` / `mcpPort` and broadcast
//! `settings:changed` in addition to the controller's `mcp:status` events.
//! The ChatGPT tunnel is desktop-only in web v1.

use serde::Deserialize;
use serde_json::{json, Value};

use agentero_core::features::system::settings::AppSettings;

use crate::error::{blocking, fail_msg, ok, parse_args, typed_err, typed_ok, HandlerResult};
use crate::rpc::RpcCtx;

/// Serializes `mcp_set_enabled` / `mcp_set_port` RPCs end-to-end (controller
/// mutation **and** the settings write-back). The controller already serializes
/// its listener mutations; this guard also keeps the persisted `mcpPort`
/// consistent with the final listener when two admin calls overlap.
static MCP_CONTROL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetEnabledArgs {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetPortArgs {
    port: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetVaultArgs {
    #[serde(default)]
    vault_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetKnownVaultsArgs {
    #[serde(default)]
    vaults: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetParentDirArgs {
    #[serde(default)]
    parent_dir: String,
}

/// Persist one mutation of the shared settings store and broadcast
/// `settings:changed` (same envelope as `settings_set`). Returns `false` when
/// the value was already current (no write, no broadcast).
async fn persist_settings(
    ctx: &RpcCtx,
    mutate: impl FnOnce(&mut AppSettings) -> bool + Send + 'static,
) -> Result<bool, String> {
    let store = ctx.state.settings.clone();
    let settings = blocking(move || {
        let mut settings = store.get().map_err(|error| error.to_string())?.settings;
        if !mutate(&mut settings) {
            return Ok(None);
        }
        store.set(settings).map(Some).map_err(|error| error.to_string())
    })
    .await?;
    match settings {
        Some(settings) => {
            ctx.state.events.emit("settings:changed", &settings);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// `mcp_get_status` → `ApiResult<McpStatus>`.
pub async fn get_status(ctx: &RpcCtx) -> HandlerResult {
    ok(ctx.state.mcp.status())
}

/// `mcp_set_enabled({enabled})` → `ApiResult<McpStatus>` (typed command, like
/// the desktop shell). Persists `mcpEnabled`.
pub async fn set_enabled(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SetEnabledArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let _control = MCP_CONTROL_LOCK.lock().await;
    let controller = ctx.state.mcp.clone();
    let status = controller.set_enabled(args.enabled).await;
    if let Err(error) = persist_settings(ctx, move |settings| {
        if settings.mcp_enabled == args.enabled {
            return false;
        }
        settings.mcp_enabled = args.enabled;
        true
    })
    .await
    {
        log::warn!("mcp_set_enabled: persist settings failed: {error}");
    }
    typed_ok(status)
}

/// `mcp_set_port({port})` → `ApiResult<McpStatus>` (typed command).
///
/// A zero port keeps the current listener and reports through `lastError`;
/// otherwise the new port is persisted and the listener restarts immediately.
pub async fn set_port(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SetPortArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let _control = MCP_CONTROL_LOCK.lock().await;
    let controller = ctx.state.mcp.clone();
    let status = controller.set_port(args.port).await;
    let accepted = args.port != 0 && status.port == args.port;
    if accepted {
        if let Err(error) = persist_settings(ctx, move |settings| {
            if settings.mcp_port == args.port {
                return false;
            }
            settings.mcp_port = args.port;
            true
        })
        .await
        {
            log::warn!("mcp_set_port: persist settings failed: {error}");
        }
    }
    typed_ok(status)
}

/// `mcp_set_vault({vaultPath})` → `ApiResult<null>`.
pub async fn set_vault(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SetVaultArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    ctx.state.mcp.set_vault(args.vault_path);
    ok(Value::Null)
}

/// `mcp_set_known_vaults({vaults})` → `ApiResult<null>`.
pub async fn set_known_vaults(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SetKnownVaultsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    ctx.state.mcp.set_known_vaults(args.vaults);
    ok(Value::Null)
}

/// `mcp_set_parent_dir({parentDir})` → `ApiResult<null>`.
pub async fn set_parent_dir(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SetParentDirArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let parent_dir = agentero_core::fs::normalize_rel_separators(args.parent_dir.trim());
    if !parent_dir.is_empty() {
        ctx.state.mcp.set_parent_dir(parent_dir);
    }
    ok(Value::Null)
}

/// Unavailable tunnel status; keeps the desktop `McpTunnelStatus` shape so the
/// settings pane can hide it without special-casing the payload.
fn unavailable_tunnel_status(message: &str) -> Value {
    json!({
        "phase": "binaryMissing",
        "running": false,
        "pid": null,
        "mcpUrl": null,
        "installCommand": "",
        "lastError": message,
    })
}

const TUNNEL_UNAVAILABLE: &str =
    "MCP tunnel is not available in the headless web host (serve /mcp behind a reverse proxy)";

/// `mcp_tunnel_status` → `ApiResult<McpTunnelStatus>`; web v1 reports the
/// tunnel as unavailable instead of spawning `tunnel-client`.
pub async fn tunnel_status(_ctx: &RpcCtx) -> HandlerResult {
    ok(unavailable_tunnel_status(TUNNEL_UNAVAILABLE))
}

/// `mcp_tunnel_start` — desktop-only supervisor; web returns a business error.
pub async fn tunnel_start(_ctx: &RpcCtx) -> HandlerResult {
    typed_err(TUNNEL_UNAVAILABLE)
}

/// `mcp_tunnel_stop` — nothing to stop on the web host.
pub async fn tunnel_stop(_ctx: &RpcCtx) -> HandlerResult {
    typed_ok(unavailable_tunnel_status(TUNNEL_UNAVAILABLE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_status_shape_matches_desktop_bindings() {
        let value = unavailable_tunnel_status(TUNNEL_UNAVAILABLE);
        let object = value.as_object().expect("object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "installCommand",
                "lastError",
                "mcpUrl",
                "phase",
                "pid",
                "running"
            ]
        );
        assert_eq!(value["phase"], "binaryMissing");
        assert_eq!(value["running"], false);
        assert!(value["lastError"].as_str().unwrap().contains("not available"));
    }
}
