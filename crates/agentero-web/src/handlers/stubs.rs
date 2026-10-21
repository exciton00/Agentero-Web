//! Commands that the web host intentionally stubs while the domain is ported.
//!
//! Every entry mirrors the desktop wire shape so the SPA boots cleanly:
//! - plain `ApiResult<T>` commands answer a success or business-error envelope;
//! - typed commands answer the inner `ApiResult` envelope (their `Result<_, String>`
//!   transport rejection is reserved for real failures).
//!
//! Unknown names fall back to a typed "not implemented" business error, which
//! both `callApi` and `callApiResult` helpers surface as a normal Host error.

use serde_json::{json, Value};

use crate::error::{fail_msg, ok, typed_err, typed_ok, HandlerResult};
use crate::rpc::RpcCtx;

fn disabled_connector_status() -> Value {
    json!({
        "enabled": false,
        "listening": false,
        "port": 23119,
        "boundAddress": null,
        "lastError": null,
        "vaultPath": null,
        "parentDir": "papers",
    })
}

fn disabled_mcp_status() -> Value {
    json!({
        "enabled": false,
        "listening": false,
        "port": 8765,
        "url": null,
        "lastError": null,
        "vaultPath": null,
    })
}

fn empty_layout_model_status() -> Value {
    json!({
        "ready": false,
        "path": "",
        "sizeBytes": 0,
        "source": null,
        "fileName": "",
    })
}

pub async fn dispatch(_ctx: &RpcCtx, command: &str, _args: &Value) -> HandlerResult {
    match command {
        // ---- BYOK-only build: no compiled-in provider credentials ----
        "builtin_provider_status" => ok(json!({
            "available": false,
            "baseUrl": "",
            "translateModel": "",
            "embeddingModel": "",
            "ocrModel": "",
        })),
        // System font enumeration is a desktop capability; the browser uses
        // the macOS/Windows font stack it already ships with.
        "list_system_fonts" => ok(Vec::<String>::new()),
        "network_system_proxy" => ok(Option::<String>::None),

        // ---- Desktop menu locale: the browser owns its own i18n ----
        "set_locale" => typed_ok(Value::Null),

        // ---- Open requests (deep link / CLI): nothing pending on a server ----
        "ui_request_take_pending" => ok(Option::<Value>::None),

        // ---- Jobs: the server-side task center lands in M2 ----
        "job_list" => typed_ok(Vec::<Value>::new()),
        "job_reconcile_vault" => typed_ok(0u32),
        "paper_ingest_reconcile" => typed_ok(0u32),
        "job_model_download_enqueue" => {
            typed_err("layout model download is not available in the headless web host")
        }

        // ---- Agent registry: ACP support lands in M3 ----
        "agent_list_agents" => ok(json!({
            "agents": [],
            "defaultId": null,
            "enabled": false,
        })),
        "agent_scan_catalog" => ok(json!({
            "entries": [],
            "customAgents": [],
            "defaultId": null,
            "enabled": false,
            "proxyEnabled": false,
            "proxyUrl": "",
            "userAgent": "",
            "userAgentProviderIds": "",
        })),
        "agent_list_skills" => typed_ok(Vec::<Value>::new()),

        // ---- Layout analysis (ONNX): deferred (see docs/development/webui.md) ----
        "layout_model_status" => ok(empty_layout_model_status()),

        // ---- Discovery / feeds: no server-side discovery in v1 ----
        "feeds_items" => ok(json!({ "items": [] })),
        "recommend_arxiv" => {
            fail_msg("arXiv recommendations are not available in the headless web host")
        }
        "recommend_arxiv_last" => ok(Option::<Value>::None),

        // ---- Desktop-only integrations: reported as disabled ----
        "connector_set_vault" => ok(Value::Null),
        "connector_set_enabled" => typed_ok(disabled_connector_status()),
        "connector_status" => ok(disabled_connector_status()),
        "mcp_set_vault" => ok(Value::Null),
        "mcp_set_enabled" => typed_ok(disabled_mcp_status()),
        "mcp_status" => ok(disabled_mcp_status()),
        "bridge_status" => ok(json!({ "enabled": false })),

        // ---- LaTeX: server image support lands with the deployment milestone ----
        "detect_latex_engines" => ok(Vec::<Value>::new()),
        "export_system_cjk_font" => {
            fail_msg("system CJK font export is not available in the headless web host")
        }

        // ---- Tauri-only surface ----
        "doctor_set_dirty_paths" => ok(Value::Null),
        "fs_watch_start" => typed_ok(Value::Null),
        "fs_watch_stop" => typed_ok(Value::Null),

        other => fail_msg(format!(
            "web host: command \"{other}\" is not implemented yet"
        )),
    }
}
