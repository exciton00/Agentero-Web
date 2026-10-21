//! Experimental BibTeX / BibLaTeX network mount.
//!
//! `GET /agentero_online.bib` serves the current Vault's bibliography for
//! reference managers (Zotero "Add by URL", JabRef, Overleaf, …). One stable
//! URL is mounted; the output dialect follows `settings.bibtexMount.format`
//! (`bibtex` or `biblatex`). Rendering is local
//! (`agentero_core::features::paper::catalog::bibtex_export`): metadata is never
//! sent to a cloud translation service, which matters for self-hosted users.
//!
//! Contract:
//! - disabled → `404 {"error":{"code":"not_mounted"}}`;
//! - enabled → `200` `application/x-bibtex; charset=utf-8`, `Cache-Control:
//!   no-store`, `X-Agentero-Bib-Count: <n>` (empty body when nothing matches);
//! - `GET /api/bibtex/status` → `{enabled, format, tags, dirs, count,
//!   generatedAt, path}`, where `path` is always [`MOUNT_PATH`].

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use agentero_core::features::paper::catalog::bibtex_export::{self, BibFormat};
use agentero_core::features::system::settings::BibtexMountSettings;

use crate::state::AppState;

/// Stable mount path served to reference managers.
pub const MOUNT_PATH: &str = "/agentero_online.bib";

/// Served content type (UTF-8 is the documented, intentional encoding for both
/// dialects; the bibtex dialect escapes TeX specials instead of losing Unicode).
const BIB_CONTENT_TYPE: &str = "application/x-bibtex; charset=utf-8";

/// Pure mount gate (unit-tested).
fn mount_enabled(mount: &BibtexMountSettings) -> bool {
    mount.enabled
}

fn not_mounted() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": {
                "code": "not_mounted",
                "message": "the bibliography network mount is not enabled in Settings → Experimental",
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

async fn serve(state: Arc<AppState>) -> Response {
    let mount = state.settings.bibtex_mount();
    if !mount_enabled(&mount) {
        return not_mounted();
    }
    // The mounted URL is stable; the settings choose the serialization dialect.
    let format = BibFormat::parse(&mount.format);
    let vault = state.config.vault_root.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        bibtex_export::render(&vault, format, &mount).map_err(|error| error.to_string())
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

/// `GET /agentero_online.bib` (the mounted bibliography; dialect from settings).
pub async fn online_bib_handler(State(state): State<Arc<AppState>>) -> Response {
    serve(state).await
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
        "path": MOUNT_PATH,
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
    fn single_stable_mount_path() {
        assert_eq!(MOUNT_PATH, "/agentero_online.bib");
        assert!(MOUNT_PATH.ends_with(".bib"));
    }

    #[test]
    fn mount_gate_only_checks_the_master_switch() {
        assert!(mount_enabled(&mount(true, "bibtex")));
        assert!(mount_enabled(&mount(true, "biblatex")));
        assert!(!mount_enabled(&mount(false, "bibtex")));
    }

    #[test]
    fn dialect_follows_the_settings_format() {
        // Legacy/unknown values fall back to bibtex, matching settings normalize.
        assert_eq!(BibFormat::parse("bibtex"), BibFormat::Bibtex);
        assert_eq!(BibFormat::parse("biblatex"), BibFormat::Biblatex);
        assert_eq!(BibFormat::parse(" BiblAteX "), BibFormat::Biblatex);
        assert_eq!(BibFormat::parse("ris"), BibFormat::Bibtex);
        assert_eq!(BibFormat::parse(""), BibFormat::Bibtex);
    }
}
