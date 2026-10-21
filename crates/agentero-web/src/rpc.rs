//! HTTP RPC dispatcher.
//!
//! One endpoint (`POST /api/rpc/{command}`) replaces the Tauri `invoke`
//! channel: the command name is the desktop command name and the body is the
//! same argument object the generated bindings send. Handlers answer with the
//! exact JSON the desktop command would return.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::HandlerResult;
use crate::handlers;
use crate::state::AppState;

/// Per-request context: the state plus the browser tab that issued the call.
pub struct RpcCtx {
    pub state: Arc<AppState>,
    /// Browser tab label (`X-Agentero-Window`); targeted events go here.
    pub window: Option<String>,
}

#[derive(Debug)]
pub struct RpcRequest {
    pub command: String,
    pub args: Value,
}

/// `POST /api/rpc/{command}`
pub async fn rpc_handler(
    State(state): State<Arc<AppState>>,
    Path(command): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let args: Value = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("invalid_json: {error}") })),
                )
                    .into_response();
            }
        }
    };
    let window = headers
        .get("x-agentero-window")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let ctx = RpcCtx { state, window };
    let started = std::time::Instant::now();
    let result = dispatch(&ctx, &command, &args).await;
    log::debug!(
        "rpc {command} args={} took={}ms",
        summarise(&args),
        started.elapsed().as_millis()
    );
    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": message })),
        )
            .into_response(),
    }
}

fn summarise(args: &Value) -> String {
    let text = args.to_string();
    if text.len() <= 160 {
        text
    } else {
        format!("{}…", &text[..160])
    }
}

/// Route a command name to its handler; unknown names fall through to the
/// stub table so the SPA can boot while domains are still being ported.
pub async fn dispatch(ctx: &RpcCtx, command: &str, args: &Value) -> HandlerResult {
    // Domain handlers first: feature work extends this without touching the
    // platform dispatch table.
    if let Some(result) = handlers::domain::dispatch(ctx, command, args).await {
        return result;
    }
    if let Some(result) = handlers::runtime::dispatch(ctx, command, args).await {
        return result;
    }
    match command {
        // Settings (shared XDG settings.json)
        "settings_get" => handlers::settings::get(ctx).await,
        "settings_set" => handlers::settings::set(ctx, args).await,

        // Vault lifecycle + tree
        "vault_create" => handlers::vault::create(ctx, args).await,
        "vault_ensure" => handlers::vault::ensure(ctx, args).await,
        "vault_tree_build" => handlers::vault::tree_build(ctx, args).await,
        "vault_tree_children" => handlers::vault::tree_children(ctx, args).await,
        "vault_allow_fs_scope" => handlers::vault::allow_fs_scope(ctx, args).await,
        "vault_open_take_pending" => handlers::vault::take_pending_open(ctx).await,

        // Paper catalog
        "paper_list" => handlers::paper::list(ctx, args).await,
        "paper_get" => handlers::paper::get(ctx, args).await,
        "paper_page_counts" => handlers::paper::page_counts(ctx, args).await,
        "paper_reading_activity_batch" => handlers::paper::reading_activity_batch(ctx, args).await,

        other => handlers::stubs::dispatch(ctx, other, args).await,
    }
}
