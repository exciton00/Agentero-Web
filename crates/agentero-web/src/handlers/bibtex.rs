//! Experimental BibTeX / BibLaTeX network mount.
//!
//! `GET /bibtex` and `GET /biblatex` serve the current Vault's bibliography for
//! reference managers (Zotero "Add by URL", JabRef, Overleaf, …). Rendering is
//! local (`agentero_core::features::paper::catalog::bibtex_export`): metadata is
//! never sent to a cloud translation service, which matters for self-hosted
//! users.
//!
//! Contract:
//! - disabled mount or dialect mismatch → `404 {"error":{"code":"not_mounted"}}`;
//! - enabled → `200` `application/x-bibtex; charset=utf-8`, `Cache-Control:
//!   no-store`, `X-Agentero-Bib-Count: <n>` (empty body when nothing matches);
//! - `GET /api/bibtex/status` → `{enabled, format, tags, dirs, count,
//!   generatedAt, path}` for the Experimental settings pane.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use agentero_core::features::paper::catalog::bibtex_export::{self, BibFormat};
use agentero_core::features::system::settings::BibtexMountSettings;

use crate::state::AppState;

/// Served content type (UTF-8 is the documented, intentional encoding for both
/// dialects; the bibtex dialect escapes TeX specials instead of losing Unicode).
const BIB_CONTENT_TYPE: &str = "application/x-bibtex; charset=utf-8";

/// Pure mount gate (unit-tested): enabled AND the request path matches the
/// configured dialect.
fn mount_allows(mount: &BibtexMountSettings, requested: BibFormat) -> bool {
    mount.enabled && mount.format.trim().eq_ignore_ascii_case(requested.as_str())
}

fn not_mounted(requested: BibFormat) -> Response {
    let message = format!(
        "the {} network mount is not enabled in Settings → Experimental",
        requested.as_str()
    );
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": {
                "code": "not_mounted",
                "message": message,
            }
        })),
    )
        .into_response()
}

fn internal_error(message: impl Into<String>) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": {
                "code": "bibtex_render_failed",
                "message": message.into(),
            }
        })),
    )
        .into_response()
}

async fn serve(state: Arc<AppState>, requested: BibFormat) -> Response {
    let mount = state.settings.bibtex_mount();
    if !mount_allows(&mount, requested) {
        return not_mounted(requested);
    }
    let vault = state.config.vault_root.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        bibtex_export::render(&vault, requested, &mount).map_err(|error| error.to_string())
    })
    .await;
    let rendered = match rendered {
        Ok(Ok(rendered)) => rendered,
        Ok(Err(error)) => return internal_error(error),
        Err(error) => return internal_error(format!("bibtex task failed: {error}")),
    };
    let mut response = (StatusCode::OK, rendered.body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(BIB_CONTENT_TYPE),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(count) = HeaderValue::from_str(&rendered.count.to_string()) {
        response
            .headers_mut()
            .insert("x-agentero-bib-count", count);
    }
    response
}

/// `GET /bibtex`
pub async fn bibtex_handler(State(state): State<Arc<AppState>>) -> Response {
    serve(state, BibFormat::Bibtex).await
}

/// `GET /biblatex`
pub async fn biblatex_handler(State(state): State<Arc<AppState>>) -> Response {
    serve(state, BibFormat::Biblatex).await
}

/// `GET /api/bibtex/status`
pub async fn status_handler(State(state): State<Arc<AppState>>) -> Response {
    let mount = state.settings.bibtex_mount();
    let requested = BibFormat::parse(&mount.format);
    let vault = state.config.vault_root.clone();
    let render_mount = mount.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        bibtex_export::render(&vault, requested, &render_mount).map_err(|error| error.to_string())
    })
    .await;
    let count = match rendered {
        Ok(Ok(rendered)) => rendered.count,
        Ok(Err(error)) => return internal_error(error),
        Err(error) => return internal_error(format!("bibtex task failed: {error}")),
    };
    Json(json!({
        "enabled": mount.enabled,
        "format": requested.as_str(),
        "tags": mount.tags,
        "dirs": mount.dirs,
        "count": count,
        "generatedAt": agentero_core::time::now_rfc3339_millis(),
        "path": requested.mount_path(),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(enabled: bool, format: &str) -> BibtexMountSettings {
        BibtexMountSettings {
            enabled,
            format: format.to_string(),
            tags: Vec::new(),
            dirs: Vec::new(),
        }
    }

    #[test]
    fn mount_gate_requires_enabled_and_matching_dialect() {
        assert!(mount_allows(&mount(true, "bibtex"), BibFormat::Bibtex));
        assert!(!mount_allows(&mount(true, "bibtex"), BibFormat::Biblatex));
        assert!(!mount_allows(&mount(false, "bibtex"), BibFormat::Bibtex));
        assert!(mount_allows(&mount(true, "BibLaTeX"), BibFormat::Biblatex));
        // Unknown formats normalize to bibtex upstream; the raw gate still only
        // matches the normalized dialect path.
        assert!(!mount_allows(&mount(true, "ris"), BibFormat::Biblatex));
    }
}
