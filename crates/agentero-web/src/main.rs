//! Headless web host for Agentero.
//!
//! Serves the existing React SPA and exposes the desktop Host command surface
//! over HTTP + WebSocket by bridging to the tauri-free `agentero-core` crate.
//! See `docs/development/webui.md`.

// Scaffolding types are used as the port grows; keep the build warning-free.
#![allow(dead_code)]

mod agent_runtime;
mod config;
mod error;
mod events;
mod handlers;
mod rpc;
mod state;
mod static_files;
mod tasks;
mod watcher;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::Router;

use crate::config::Config;
use crate::state::AppState;

fn main() {
    // Domain code re-executes this binary as an isolated PDFium worker
    // (`spawn_pdf_worker` in agentero-core). That branch builds its own tokio
    // runtime, so it must run before `#[tokio::main]` creates one.
    if let Some(status) =
        agentero_core::features::paper::analyze::parse::try_run_pdf_parse_worker()
    {
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
        .route("/config", get(config_handler));

    let app = Router::new()
        .nest("/api", api)
        .fallback(static_files::serve)
        .with_state(state);

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
