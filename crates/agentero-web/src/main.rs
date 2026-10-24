//! Headless web host for Agentero.
//!
//! Serves the existing React SPA and exposes the desktop Host command surface
//! over HTTP + WebSocket by bridging to the tauri-free `agentero-core` crate.
//! See `docs/development/webui.md`.

// Scaffolding types are used as the port grows; keep the build warning-free.
#![allow(dead_code)]

mod agent_runtime;
mod config;
mod context_mcp;
mod error;
mod events;
mod handlers;
mod host_agent;
mod mcp;
mod parser_engines;
mod rag_mcp;
mod rpc;
mod state;
mod static_files;
mod tasks;
mod watcher;
mod rag;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, Extensions, HeaderMap, StatusCode, Version};
use axum::routing::{get, post};
use axum::Router;
use tower_http::compression::CompressionLayer;

use crate::config::Config;
use crate::state::AppState;

/// Environment override for the `/api` request-body cap, in MiB.
const ENV_MAX_BODY_MB: &str = "AGENTERO_WEB_MAX_BODY_MB";
/// Match the desktop Host's layout-analysis upload ceiling: a 15-page PDF is
/// ~10-30 MB once base64-encoded, far above axum's 2 MB default.
const DEFAULT_MAX_BODY_MB: usize = 64;
const MIN_MAX_BODY_MB: usize = 8;
const MAX_MAX_BODY_MB: usize = 512;

/// Resolve the `/api` body cap from `AGENTERO_WEB_MAX_BODY_MB`.
///
/// Out-of-range or non-numeric values fall back to the default instead of
/// failing startup: a typo should not lock the user out of the server.
fn resolve_max_body_mb(raw: Option<&str>) -> usize {
    let Some(raw) = raw else {
        return DEFAULT_MAX_BODY_MB;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_MAX_BODY_MB;
    }
    match trimmed.parse::<usize>() {
        Ok(mb) if (MIN_MAX_BODY_MB..=MAX_MAX_BODY_MB).contains(&mb) => mb,
        _ => DEFAULT_MAX_BODY_MB,
    }
}

fn max_body_mb_from_env() -> usize {
    let raw = std::env::var(ENV_MAX_BODY_MB).ok();
    let mb = resolve_max_body_mb(raw.as_deref());
    let valid = raw.as_deref().is_none_or(|raw| {
        let trimmed = raw.trim();
        trimmed.is_empty()
            || trimmed
                .parse::<usize>()
                .is_ok_and(|mb| (MIN_MAX_BODY_MB..=MAX_MAX_BODY_MB).contains(&mb))
    });
    if !valid {
        log::warn!(
            "{ENV_MAX_BODY_MB} is invalid (expected {MIN_MAX_BODY_MB}..={MAX_MAX_BODY_MB} MiB); using {mb} MiB"
        );
    }
    mb
}

/// Which response MIME types the compression layer may gzip/brotli.
///
/// PDFs, images, video and other already-compressed payloads are excluded:
/// recompressing them burns CPU and removes `Accept-Ranges`, which the PDF
/// viewer needs for range requests.
fn compressible(
    status: StatusCode,
    _version: Version,
    headers: &HeaderMap,
    _extensions: &Extensions,
) -> bool {
    if status != StatusCode::OK {
        return false;
    }
    if headers.contains_key(header::CONTENT_ENCODING) || headers.contains_key(header::CONTENT_RANGE)
    {
        return false;
    }
    let Some(content_type) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime == "text/event-stream" {
        return false;
    }
    mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/manifest+json"
                | "application/ld+json"
                | "application/javascript"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/rss+xml"
                | "application/atom+xml"
                | "application/wasm"
                | "image/svg+xml"
        )
}

fn main() {
    // Domain code re-executes this binary as an isolated PDFium worker
    // (`spawn_pdf_worker` in agentero-core). That branch builds its own tokio
    // runtime, so it must run before `#[tokio::main]` creates one.
    if let Some(status) =
        agentero_core::features::paper::analyze::parse::try_run_pdf_parse_worker()
    {
        std::process::exit(status);
    }
    // Per-session stdio RAG MCP worker spawned by an ACP agent
    // (`agentero-web --agentero-rag-mcp <config>`); builds its own runtime.
    if let Some(status) = rag_mcp::try_run_rag_mcp() {
        std::process::exit(status);
    }
    // Per-session stdio context MCP worker spawned by an ACP agent
    // (`agentero-web --agentero-context-mcp <grant>`); builds its own runtime.
    if let Some(status) = context_mcp::try_run_context_mcp() {
        std::process::exit(status);
    }
    run();
}

#[tokio::main]
async fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("agentero-web: {message}");
            std::process::exit(2);
        }
    };
    log::info!(
        "agentero-web starting addr={} vault={} dist={}",
        config.addr,
        config.vault_root.display(),
        config
            .dist_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<none>".into())
    );

    let state = Arc::new(AppState::new(config.clone()));

    // Docker host ACP: a bind-mounted Dsh is registered automatically so users
    // do not have to add a custom agent in Settings.
    host_agent::ensure_host_dsh(&state.agents, state.settings.path());

    // Process-wide cloud-sync auto scheduler: one tokio task ticking every few
    // seconds over the configured vaults (autoSync + intervalMinutes). It stops
    // with the runtime on shutdown.
    state.sync.clone().start_auto_sync();

    // MCP server: starts when settings.mcpEnabled (or AGENTERO_MCP_ENABLED=1)
    // asks for it; a bind failure is surfaced through `mcp_get_status` /
    // `mcp:status` instead of aborting startup.
    state.mcp.clone().start_if_configured().await;

    // WeKnora RAG: listens for settings changes / Vault writes to run the
    // optional debounced auto-sync (60s) and the 30-minute fallback scan.
    state.rag.clone().start();

    // RAG MCP injection for ACP agent turns: keep AgentHost's spec in sync with
    // settings (enabled + configured + tools.exposeToAgent). Skipped while off.
    sync_rag_mcp(&state);
    start_rag_mcp_settings_listener(Arc::clone(&state));

    // axum caps `Bytes` bodies at 2 MB by default, which a 15-page PDF blows
    // past once base64-encoded (`layout_remote_analyze_pdf`). The same cap
    // covers `/upload` and `PUT /api/file` so large editor saves / imports work.
    let max_body_mb = max_body_mb_from_env();
    let max_body_bytes = max_body_mb * 1024 * 1024;
    log::info!("agentero-web max body = {max_body_mb} MB");

    let api = Router::new()
        .route("/rpc/{command}", post(rpc::rpc_handler))
        .route("/events", get(events::ws_handler))
        .route(
            "/file",
            get(handlers::files::file_handler)
                .put(handlers::files::file_write_handler)
                .delete(handlers::files::file_remove_handler),
        )
        .route("/file/meta", get(handlers::files::file_meta_handler))
        .route("/dir", get(handlers::files::dir_list_handler))
        .route("/mkdir", post(handlers::files::mkdir_handler))
        .route("/rename", post(handlers::files::rename_handler))
        .route("/upload", post(handlers::files::upload_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/config", get(config_handler))
        // Experimental bibtex/biblatex network mount status (UI green light).
        .route("/bibtex/status", get(handlers::bibtex::status_handler))
        .layer(DefaultBodyLimit::max(max_body_bytes));

    let app = Router::new()
        // Experimental BibTeX/BibLaTeX network mount: one stable `.bib` URL for
        // reference managers (content dialect follows settings.bibtexMount.format);
        // must be registered before the SPA fallback.
        .route(
            handlers::bibtex::MOUNT_PATH,
            get(handlers::bibtex::online_bib_handler),
        )
        .nest("/api", api)
        .fallback(static_files::serve)
        .with_state(state)
        // gzip/brotli for JSON, Markdown, HTML and JS/CSS (the WebSocket and
        // binary responses are skipped by `compressible`). `Router::layer`
        // applies to the routes and fallback registered above.
        .layer(CompressionLayer::new().compress_when(compressible));

    let listener = match tokio::net::TcpListener::bind(config.addr).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("agentero-web: cannot bind {}: {error}", config.addr);
            std::process::exit(2);
        }
    };
    log::info!("agentero-web listening on http://{}", config.addr);

    if let Err(error) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("agentero-web: server error: {error}");
        std::process::exit(1);
    }
}

/// Build the per-turn RAG MCP spec from live settings (provider-aware).
///
/// `None` when the feature is off/unconfigured or the user did not expose the
/// tools to agents — the ACP tool surface then stays byte-identical to before.
fn rag_mcp_spec(state: &AppState) -> Option<agentero_host::models::RagMcpSpec> {
    let settings = state.settings.rag();
    if !mcp::rag_tools::agent_tools_enabled(&settings) {
        return None;
    }
    Some(agentero_host::models::RagMcpSpec {
        provider: settings.provider().id().to_string(),
        base_url: settings.base_url().to_string(),
        api_key: settings.api_key().unwrap_or_default().to_string(),
        knowledge_base_id: settings.knowledge_base_id().to_string(),
        vault_path: state.config.vault_root.to_string_lossy().into_owned(),
        state_path: agentero_core::features::rag::state::state_path(
            &state.config.vault_root,
            settings.provider(),
        )
            .to_string_lossy()
            .into_owned(),
        rerank: settings.retrieval.rerank,
        timeout_ms: settings.retrieval.timeout_ms,
        top_k: settings.retrieval.top_k,
    })
}

/// Push the current RAG MCP spec into the agent host (pool invalidated on change).
fn sync_rag_mcp(state: &Arc<AppState>) {
    let spec = rag_mcp_spec(state);
    if state.agents.set_rag_mcp(spec) {
        log::info!("agentero-web updated the agent RAG MCP configuration");
    }
}

/// Refresh the agent RAG spec whenever settings are saved from any tab.
fn start_rag_mcp_settings_listener(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut receiver = state.events.subscribe();
        loop {
            match receiver.recv().await {
                Ok(frame) if frame.event == "settings:changed" => sync_rag_mcp(&state),
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Browser bootstrap facts: default Vault root, platform marker, version.
async fn config_handler(State(state): State<Arc<AppState>>) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "platform": "web",
        "version": env!("CARGO_PKG_VERSION"),
        "vaultRoot": state.config.vault_root.to_string_lossy(),
    }))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    log::info!("agentero-web shutting down");
}

#[cfg(test)]
mod tests {
    use super::{compressible, resolve_max_body_mb, DEFAULT_MAX_BODY_MB};
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Version};

    #[test]
    fn max_body_mb_defaults_and_override_range() {
        assert_eq!(resolve_max_body_mb(None), DEFAULT_MAX_BODY_MB);
        assert_eq!(resolve_max_body_mb(Some("")), DEFAULT_MAX_BODY_MB);
        assert_eq!(resolve_max_body_mb(Some("  ")), DEFAULT_MAX_BODY_MB);
        assert_eq!(resolve_max_body_mb(Some("64")), 64);
        assert_eq!(resolve_max_body_mb(Some(" 96 ")), 96);
        assert_eq!(resolve_max_body_mb(Some("8")), 8);
        assert_eq!(resolve_max_body_mb(Some("512")), 512);
    }

    #[test]
    fn max_body_mb_invalid_values_fall_back_to_default() {
        for raw in ["7", "513", "0", "-8", "abc", "64MB", "1.5"] {
            assert_eq!(
                resolve_max_body_mb(Some(raw)),
                DEFAULT_MAX_BODY_MB,
                "invalid override {raw:?} must fall back"
            );
        }
    }

    fn headers_with(content_type: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        headers
    }

    #[test]
    fn compression_skips_binary_media() {
        let status = StatusCode::OK;
        let version = Version::HTTP_11;
        let empty = axum::http::Extensions::new();
        assert!(compressible(
            status,
            version,
            &headers_with("application/json; charset=utf-8"),
            &empty
        ));
        assert!(compressible(
            status,
            version,
            &headers_with("text/html; charset=utf-8"),
            &empty
        ));
        assert!(compressible(
            status,
            version,
            &headers_with("image/svg+xml"),
            &empty
        ));
        for mime in [
            "application/pdf",
            "image/png",
            "image/jpeg",
            "video/mp4",
            "application/octet-stream",
        ] {
            assert!(
                !compressible(status, version, &headers_with(mime), &empty),
                "{mime} must not be recompressed"
            );
        }
    }

    #[test]
    fn compression_skips_partial_and_non_ok_responses() {
        let version = Version::HTTP_11;
        let empty = axum::http::Extensions::new();
        assert!(!compressible(
            StatusCode::PARTIAL_CONTENT,
            version,
            &headers_with("application/json"),
            &empty
        ));
        assert!(!compressible(
            StatusCode::NOT_MODIFIED,
            version,
            &headers_with("application/json"),
            &empty
        ));
        let mut range_headers = headers_with("application/json");
        range_headers.insert(header::CONTENT_RANGE, HeaderValue::from_static("bytes 0-9/10"));
        assert!(!compressible(StatusCode::OK, version, &range_headers, &empty));
        let mut encoded = headers_with("application/json");
        encoded.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        assert!(!compressible(StatusCode::OK, version, &encoded, &empty));
    }
}
