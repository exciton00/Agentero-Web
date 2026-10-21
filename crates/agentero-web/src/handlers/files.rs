//! File transport endpoints used by the browser `plugin-fs` / preview shims.
//!
//! Vault files are fetched by absolute path; every request is jailed to the
//! configured Vault root (plus optional `AGENTERO_WEB_ALLOW_ROOTS`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::state::AppState;

/// Extra roots allowed by `/api/file` & `/api/upload`, colon/comma separated.
pub const ENV_ALLOW_ROOTS: &str = "AGENTERO_WEB_ALLOW_ROOTS";

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    pub path: String,
    #[serde(default)]
    pub download: Option<bool>,
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn allowed_roots(state: &Arc<AppState>) -> Vec<PathBuf> {
    let mut roots = vec![canonical(&state.config.vault_root)];
    if let Ok(value) = std::env::var(ENV_ALLOW_ROOTS) {
        for part in value.split([':', ',']) {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(canonical(Path::new(part)));
            }
        }
    }
    roots
}

fn is_allowed(state: &Arc<AppState>, path: &Path) -> bool {
    let target = canonical(path);
    allowed_roots(state)
        .iter()
        .any(|root| target == *root || target.starts_with(root))
}

/// `GET /api/file?path=<abs>[&download=1]`
pub async fn file_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FileQuery>,
) -> Response {
    let path = PathBuf::from(query.path.trim());
    if !path.is_absolute() || !is_allowed(&state, &path) {
        return (StatusCode::FORBIDDEN, "path is outside the allowed roots").into_response();
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (StatusCode::NOT_FOUND, format!("cannot read file: {error}")).into_response();
        }
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let mut response = (StatusCode::OK, bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    if query.download.unwrap_or(false) {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("download");
        if let Ok(value) =
            header::HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\""))
        {
            response
                .headers_mut()
                .insert(header::CONTENT_DISPOSITION, value);
        }
    }
    response
}

fn decode_header(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?;
    percent_decode(value)
}

/// Minimal percent-decoder for header-carried paths (non-UTF8 byte escapes
/// fall back to the raw value).
fn percent_decode(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.bytes();
    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let hi = chars.next()?;
            let lo = chars.next()?;
            let pair = [hi, lo];
            let hex = std::str::from_utf8(&pair).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

/// `POST /api/upload` with `X-Agentero-Dir` (percent-encoded absolute dir) and
/// `X-Agentero-Name` (percent-encoded file name); body is the raw file.
pub async fn upload_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(dir) = decode_header(&headers, "x-agentero-dir") else {
        return (StatusCode::BAD_REQUEST, "missing X-Agentero-Dir").into_response();
    };
    let Some(name) = decode_header(&headers, "x-agentero-name") else {
        return (StatusCode::BAD_REQUEST, "missing X-Agentero-Name").into_response();
    };
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name == ".."
        || name.starts_with('.')
    {
        return (StatusCode::BAD_REQUEST, "invalid file name").into_response();
    }
    let dir = PathBuf::from(dir);
    if !dir.is_absolute() || !is_allowed(&state, &dir) {
        return (
            StatusCode::FORBIDDEN,
            "target dir is outside the allowed roots",
        )
            .into_response();
    }
    if let Err(error) = tokio::fs::create_dir_all(&dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot create dir: {error}"),
        )
            .into_response();
    }
    let target = dir.join(&name);
    if let Err(error) = tokio::fs::write(&target, &body).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot write file: {error}"),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        axum::Json(json!({ "path": target.to_string_lossy() })),
    )
        .into_response()
}
