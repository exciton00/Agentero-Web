//! Headless web host for Agentero.
//!
//! Serves the existing React SPA and exposes the desktop Host command surface
//! over HTTP + WebSocket by bridging to the tauri-free `agentero-core` crate.
//! See `docs/development/webui.md`.

// Scaffolding types are used as the port grows; keep the build warning-free.
#![allow(dead_code)]

mod config;
mod error;
mod events;
mod handlers;
mod rpc;
mod state;
mod static_files;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::config::Config;
use crate::state::AppState;

#[tokio::main]
async fn main() {
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
        .route("/file", get(handlers::files::file_handler))
        .route("/upload", post(handlers::files::upload_handler))
        .route("/healthz", get(|| async { "ok" }));

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
