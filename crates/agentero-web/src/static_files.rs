//! Static SPA serving with history fallback.
//!
//! The built frontend (`pnpm build:web`) is served from `AGENTERO_WEB_DIST`.
//! Unknown GET paths fall back to `index.html` so the client router works.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

const INDEX: &str = "index.html";

pub async fn serve(State(state): State<Arc<AppState>>, method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response();
    }

    let Some(dist) = state.config.dist_dir.clone() else {
        return not_built_page();
    };

    let relative = uri.path().trim_start_matches('/');
    if let Some(path) = resolve_asset(&dist, relative) {
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return asset_response(&path, bytes, method == Method::HEAD);
        }
    }

    // History fallback: every unknown route renders the SPA.
    match tokio::fs::read(dist.join(INDEX)).await {
        Ok(bytes) => {
            let mut response = (StatusCode::OK, bytes).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            response
        }
        Err(_) => not_built_page(),
    }
}

/// Resolve a request path inside `dist`, rejecting traversal and hidden files.
fn resolve_asset(dist: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() || relative.starts_with("api/") {
        return None;
    }
    let mut out = PathBuf::new();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str()?;
                if part.starts_with('.') {
                    return None;
                }
                out.push(part);
            }
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        return None;
    }
    let candidate = dist.join(&out);
    candidate.is_file().then_some(candidate)
}

fn asset_response(path: &Path, bytes: Vec<u8>, head: bool) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let body = if head { Vec::new() } else { bytes };
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    // Vite hashes asset file names; everything under /assets is immutable.
    let cache = if path.components().any(|part| part.as_os_str() == "assets") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

fn not_built_page() -> Response {
    let html = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Agentero Web</title>
<style>body{font:14px/1.6 system-ui;margin:48px;max-width:720px;color:#111}
code{background:#f3f3f3;padding:2px 6px;border-radius:4px}</style></head>
<body><h2>Agentero Web host is running</h2>
<p>The frontend build was not found. Build it and point the server at it:</p>
<pre><code>pnpm install
pnpm build:web
AGENTERO_WEB_DIST=dist AGENTERO_WEB_VAULT=~/Agentero cargo run -p agentero-web</code></pre>
<p>API endpoints are live: <code>POST /api/rpc/&lt;command&gt;</code>, <code>GET /api/events</code>, <code>GET /api/file?path=…</code>.</p>
</body></html>"#;
    let mut response = (StatusCode::OK, html).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}
