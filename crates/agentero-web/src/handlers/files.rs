//! File transport endpoints backing the browser `plugin-fs` / preview shims.
//!
//! Every path is absolute and jailed to the configured Vault root plus any
//! roots from `AGENTERO_WEB_ALLOW_ROOTS` (single-user server; auth lives in a
//! reverse proxy).
//!
//! | Route | Purpose |
//! |---|---|
//! | `GET /api/file?path=` | read bytes (`?download=1` for attachment) |
//! | `PUT /api/file?path=` | write bytes (parents created) |
//! | `DELETE /api/file?path=[&recursive=1]` | remove file/dir |
//! | `GET /api/file/meta?path=` | `{exists,isFile,isDirectory,size,mtimeMs}` |
//! | `GET /api/dir?path=` | directory entries |
//! | `POST /api/mkdir?path=[&recursive=1]` | create directory |
//! | `POST /api/rename?from=&to=` | rename within the jail |
//! | `POST /api/upload` | upload with `X-Agentero-Dir` / `X-Agentero-Name` |

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::state::AppState;

/// Extra roots allowed by the file endpoints, colon/comma separated.
pub const ENV_ALLOW_ROOTS: &str = "AGENTERO_WEB_ALLOW_ROOTS";

/// Lenient query fields: the browser shim sends `1`/`0` for booleans, while
/// curl users may send `true`/`false`; both are accepted.
#[derive(Debug, Default, Deserialize)]
pub struct PathQuery {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub recursive: Option<String>,
    #[serde(default)]
    pub download: Option<String>,
}

fn parse_flag(value: Option<&String>) -> bool {
    match value.map(|raw| raw.trim().to_ascii_lowercase()) {
        Some(raw) => matches!(raw.as_str(), "1" | "true" | "yes" | "on"),
        None => false,
    }
}

#[allow(clippy::result_large_err)]
fn require_path(query: &PathQuery) -> Result<PathBuf, Response> {
    let Some(raw) = query.path.as_deref().filter(|path| !path.trim().is_empty()) else {
        return Err((StatusCode::BAD_REQUEST, "missing path").into_response());
    };
    jail_path(raw)
}

#[derive(Debug, Deserialize)]
pub struct RenameQuery {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    pub path: String,
    pub exists: bool,
    pub is_file: bool,
    pub is_directory: bool,
    pub size: u64,
    pub mtime_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub is_file: bool,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub mtime_ms: Option<u64>,
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

/// Validate an absolute path without checking the jail roots.
#[allow(clippy::result_large_err)]
fn jail_path(raw: &str) -> Result<PathBuf, Response> {
    let path = PathBuf::from(raw.trim());
    if !path.is_absolute() {
        return Err((StatusCode::BAD_REQUEST, "path must be absolute").into_response());
    }
    Ok(path)
}

/// Validate an absolute path inside the jail.
#[allow(clippy::result_large_err)]
fn jail(state: &Arc<AppState>, raw: &str) -> Result<PathBuf, Response> {
    let path = jail_path(raw)?;
    if !is_allowed(state, &path) {
        return Err((StatusCode::FORBIDDEN, "path is outside the allowed roots").into_response());
    }
    Ok(path)
}

fn io_error(status: StatusCode, what: &str, error: std::io::Error) -> Response {
    (status, format!("{what}: {error}")).into_response()
}

fn mtime_ms(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
}

/// `GET /api/file?path=<abs>[&download=1]`
pub async fn file_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
) -> Response {
    let path = match require_path(&query) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !is_allowed(&state, &path) {
        return (StatusCode::FORBIDDEN, "path is outside the allowed roots").into_response();
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => return io_error(StatusCode::NOT_FOUND, "cannot read file", error),
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
    if parse_flag(query.download.as_ref()) {
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

/// `PUT /api/file?path=<abs>` — write bytes, creating parent directories.
pub async fn file_write_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
    body: Bytes,
) -> Response {
    let path = match require_path(&query) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !is_allowed(&state, &path) {
        return (StatusCode::FORBIDDEN, "path is outside the allowed roots").into_response();
    }
    if let Some(parent) = path.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            return io_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot create parents",
                error,
            );
        }
    }
    match tokio::fs::write(&path, &body).await {
        Ok(()) => Json(json!({ "ok": true, "path": path.to_string_lossy() })).into_response(),
        Err(error) => io_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot write file",
            error,
        ),
    }
}

/// `DELETE /api/file?path=<abs>[&recursive=1]`
pub async fn file_remove_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
) -> Response {
    let path = match require_path(&query) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !is_allowed(&state, &path) {
        return (StatusCode::FORBIDDEN, "path is outside the allowed roots").into_response();
    }
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) => return io_error(StatusCode::NOT_FOUND, "cannot stat path", error),
    };
    let result = if meta.is_dir() {
        if parse_flag(query.recursive.as_ref()) {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_dir(&path).await
        }
    } else {
        tokio::fs::remove_file(&path).await
    };
    match result {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(error) => io_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot remove", error),
    }
}

/// `GET /api/file/meta?path=<abs>`
pub async fn file_meta_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
) -> Response {
    let path = match require_path(&query) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !is_allowed(&state, &path) {
        return (StatusCode::FORBIDDEN, "path is outside the allowed roots").into_response();
    }
    let meta = std::fs::symlink_metadata(&path).ok();
    Json(FileMeta {
        path: path.to_string_lossy().to_string(),
        exists: meta.is_some(),
        is_file: meta.as_ref().is_some_and(|meta| meta.is_file()),
        is_directory: meta.as_ref().is_some_and(|meta| meta.is_dir()),
        size: meta.as_ref().map(|meta| meta.len()).unwrap_or(0),
        mtime_ms: meta.as_ref().and_then(mtime_ms),
    })
    .into_response()
}

/// `GET /api/dir?path=<abs>`
pub async fn dir_list_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
) -> Response {
    let path = match require_path(&query) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !is_allowed(&state, &path) {
        return (StatusCode::FORBIDDEN, "path is outside the allowed roots").into_response();
    }
    let mut entries = Vec::new();
    let mut read_dir = match tokio::fs::read_dir(&path).await {
        Ok(read_dir) => read_dir,
        Err(error) => return io_error(StatusCode::NOT_FOUND, "cannot read dir", error),
    };
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let file_type = entry.file_type().await.ok();
        entries.push(DirEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            is_file: meta.is_file(),
            is_directory: meta.is_dir(),
            is_symlink: file_type.is_some_and(|kind| kind.is_symlink()),
            size: meta.len(),
            mtime_ms: mtime_ms(&meta),
        });
    }
    entries.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then(a.name.cmp(&b.name))
    });
    Json(entries).into_response()
}

/// `POST /api/mkdir` — accepts either `?path=&recursive=` or
/// `{"path": "...", "recursive": true}` (the browser shim sends JSON).
pub async fn mkdir_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
    body: Bytes,
) -> Response {
    let payload: serde_json::Value = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)
    };
    let raw_path = query.path.clone().or_else(|| {
        payload
            .get("path")
            .and_then(|value| value.as_str())
            .map(str::to_string)
    });
    let Some(raw_path) = raw_path.as_deref().filter(|path| !path.trim().is_empty()) else {
        return (StatusCode::BAD_REQUEST, "missing path").into_response();
    };
    let path = match jail(&state, raw_path) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let recursive = query
        .recursive
        .as_deref()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .or_else(|| payload.get("recursive").and_then(|value| value.as_bool()))
        .unwrap_or(true);
    let result = if recursive {
        tokio::fs::create_dir_all(&path).await
    } else {
        tokio::fs::create_dir(&path).await
    };
    match result {
        Ok(()) => Json(json!({ "ok": true, "path": path.to_string_lossy() })).into_response(),
        Err(error) => io_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot create dir",
            error,
        ),
    }
}

/// `POST /api/rename?from=<abs>&to=<abs>`
pub async fn rename_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RenameQuery>,
) -> Response {
    let from = match jail(&state, &query.from) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let to = match jail(&state, &query.to) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if let Some(parent) = to.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            return io_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot create parents",
                error,
            );
        }
    }
    match tokio::fs::rename(&from, &to).await {
        Ok(()) => Json(json!({ "ok": true, "path": to.to_string_lossy() })).into_response(),
        Err(error) => io_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot rename", error),
    }
}

fn decode_header(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?;
    percent_decode(value)
}

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

/// `POST /api/upload` with `X-Agentero-Dir` / `X-Agentero-Name` (percent-encoded).
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
    let dir = match jail(&state, &dir) {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    if let Err(error) = tokio::fs::create_dir_all(&dir).await {
        return io_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot create dir",
            error,
        );
    }
    let target = dir.join(&name);
    match tokio::fs::write(&target, &body).await {
        Ok(()) => Json(json!({ "path": target.to_string_lossy() })).into_response(),
        Err(error) => io_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot write file",
            error,
        ),
    }
}
