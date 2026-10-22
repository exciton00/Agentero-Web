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

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::state::AppState;

/// Extra roots allowed by the file endpoints, colon/comma separated.
pub const ENV_ALLOW_ROOTS: &str = "AGENTERO_WEB_ALLOW_ROOTS";

/// Extensions whose payloads are immutable-enough to cache for an hour:
/// re-opening a PDF/photo should not re-download it. Everything else (JSON
/// sidecars, Markdown, TeX, …) is `no-cache` and must revalidate, so the
/// editor never renders a stale write.
const MEDIA_EXTENSIONS: &[&str] = &[
    "pdf", "png", "jpg", "jpeg", "webp", "gif", "svg", "mp4", "webm", "mp3", "wav",
];

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

/// `Cache-Control` for a file payload: media is reused for an hour, editable
/// sidecars must revalidate (an open editor would otherwise keep stale bytes).
fn cache_control_for(path: &Path) -> HeaderValue {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if MEDIA_EXTENSIONS.contains(&extension.as_str()) {
        HeaderValue::from_static("private, max-age=3600")
    } else {
        HeaderValue::from_static("private, no-cache")
    }
}

/// Strong-ish validator: enough to revalidate 304s without hashing the whole
/// body (mtime nanoseconds + size; a rewrite always changes at least one).
fn file_etag(meta: &std::fs::Metadata) -> Option<String> {
    let modified = meta.modified().ok()?;
    let nanos = modified.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    Some(format!("\"{nanos:x}-{:x}\"", meta.len()))
}

fn header_str<'a>(headers: &'a HeaderMap, name: header::HeaderName) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// Weak comparison per RFC 9110 §8.8.3.2: `If-None-Match` entries may carry a
/// `W/` prefix and `*` matches any current representation.
fn etag_matches(raw: &str, etag: &str) -> bool {
    raw.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

/// `If-Modified-Since` has second granularity; truncate the file mtime before
/// comparing so a request generated from our own `Last-Modified` matches.
fn not_modified_since(modified: SystemTime, since: SystemTime) -> bool {
    let second = |time: SystemTime| time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs());
    match (second(modified), second(since)) {
        (Some(modified), Some(since)) => modified <= since,
        _ => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ByteRange {
    /// No usable `Range` header: serve the whole file with 200.
    Ignore,
    /// Inclusive byte range for a 206 response.
    Partial(u64, u64),
    /// Valid syntax but unsatisfiable against the file size (416).
    Unsatisfiable,
}

/// Parse one `bytes=` range. Multi-range requests are deliberately ignored
/// (the full body is a valid answer and both PDF viewers issue single ranges).
fn parse_range_header(raw: &str, size: u64) -> ByteRange {
    let Some(spec) = raw.trim().strip_prefix("bytes=") else {
        return ByteRange::Ignore;
    };
    let spec = spec.trim();
    if spec.contains(',') {
        return ByteRange::Ignore;
    }
    let Some((start, end)) = spec.split_once('-') else {
        return ByteRange::Ignore;
    };
    let (start, end) = (start.trim(), end.trim());
    if start.is_empty() {
        // Suffix range: the last N bytes.
        let Ok(suffix) = end.parse::<u64>() else {
            return ByteRange::Ignore;
        };
        if suffix == 0 || size == 0 {
            return ByteRange::Unsatisfiable;
        }
        let length = suffix.min(size);
        return ByteRange::Partial(size - length, size - 1);
    }
    let Ok(start) = start.parse::<u64>() else {
        return ByteRange::Ignore;
    };
    if size == 0 || start >= size {
        return ByteRange::Unsatisfiable;
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        match end.parse::<u64>() {
            Ok(end) => end.min(size - 1),
            Err(_) => return ByteRange::Ignore,
        }
    };
    if start > end {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Partial(start, end)
}

fn content_type_header(path: &Path) -> HeaderValue {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    HeaderValue::from_str(mime.as_ref())
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

/// Headers shared by 200/206/304 responses: validators plus cache policy.
fn apply_validators(
    response: &mut Response,
    etag: Option<&str>,
    modified: Option<SystemTime>,
    cache_control: HeaderValue,
) {
    let headers = response.headers_mut();
    if let Some(etag) = etag.and_then(|etag| HeaderValue::from_str(etag).ok()) {
        headers.insert(header::ETAG, etag);
    }
    if let Some(modified) = modified {
        if let Ok(value) = HeaderValue::from_str(&httpdate::fmt_http_date(modified)) {
            headers.insert(header::LAST_MODIFIED, value);
        }
    }
    headers.insert(header::CACHE_CONTROL, cache_control);
}

fn file_body_response(
    status: StatusCode,
    bytes: Vec<u8>,
    path: &Path,
    download: bool,
) -> Response {
    let mut response = (status, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, content_type_header(path));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if download {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("download");
        if let Ok(value) =
            HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\""))
        {
            headers.insert(header::CONTENT_DISPOSITION, value);
        }
    }
    response
}

/// `GET /api/file?path=<abs>[&download=1]`
///
/// Supports conditional requests (`If-None-Match` / `If-Modified-Since` → 304)
/// and single `Range` requests (206) so the PDF viewer can seek without
/// re-downloading the document.
pub async fn file_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
    headers: HeaderMap,
) -> Response {
    let path = match require_path(&query) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !is_allowed(&state, &path) {
        return (StatusCode::FORBIDDEN, "path is outside the allowed roots").into_response();
    }
    let meta = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta,
        Err(error) => return io_error(StatusCode::NOT_FOUND, "cannot read file", error),
    };
    if !meta.is_file() {
        return (StatusCode::NOT_FOUND, "cannot read file: not a regular file").into_response();
    }

    let size = meta.len();
    let etag = file_etag(&meta);
    let modified = meta.modified().ok();
    let cache_control = cache_control_for(&path);
    let download = parse_flag(query.download.as_ref());

    // Conditional request: `If-None-Match` wins over `If-Modified-Since`
    // (RFC 9110 §13.1.3).
    let unchanged = match header_str(&headers, header::IF_NONE_MATCH) {
        Some(raw) => etag
            .as_deref()
            .is_some_and(|etag| etag_matches(raw, etag)),
        None => match (
            header_str(&headers, header::IF_MODIFIED_SINCE),
            modified,
            etag.as_deref(),
        ) {
            (Some(raw), Some(modified), _) => {
                httpdate::parse_http_date(raw.trim())
                    .ok()
                    .is_some_and(|since| not_modified_since(modified, since))
            }
            _ => false,
        },
    };
    if unchanged {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        apply_validators(&mut response, etag.as_deref(), modified, cache_control);
        return response;
    }

    let range = header_str(&headers, header::RANGE)
        .map(|raw| parse_range_header(raw, size))
        .unwrap_or(ByteRange::Ignore);
    match range {
        ByteRange::Partial(start, end) => {
            let length = end - start + 1;
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(error) => return io_error(StatusCode::NOT_FOUND, "cannot read file", error),
            };
            if let Err(error) = file.seek(SeekFrom::Start(start)).await {
                return io_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot read file", error);
            }
            let mut bytes = vec![0u8; length as usize];
            if let Err(error) = file.read_exact(&mut bytes).await {
                return io_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot read file", error);
            }
            let mut response = file_body_response(StatusCode::PARTIAL_CONTENT, bytes, &path, download);
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end}/{size}"))
                    .expect("range header is ASCII"),
            );
            apply_validators(&mut response, etag.as_deref(), modified, cache_control);
            response
        }
        ByteRange::Unsatisfiable => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}")).expect("size is ASCII"),
            );
            headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            apply_validators(&mut response, etag.as_deref(), modified, cache_control);
            response
        }
        ByteRange::Ignore => {
            let bytes = match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(error) => return io_error(StatusCode::NOT_FOUND, "cannot read file", error),
            };
            let mut response = file_body_response(StatusCode::OK, bytes, &path, download);
            apply_validators(&mut response, etag.as_deref(), modified, cache_control);
            response
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_control_separates_media_from_sidecars() {
        for media in [
            "/vault/paper.pdf",
            "/vault/fig.PNG",
            "/vault/photo.jpeg",
            "/vault/figure.webp",
            "/vault/plot.svg",
            "/vault/clip.mp4",
        ] {
            assert_eq!(
                cache_control_for(Path::new(media)),
                HeaderValue::from_static("private, max-age=3600"),
                "{media} should be reusable for an hour"
            );
        }
        for sidecar in [
            "/vault/papers/a/source/layout.json",
            "/vault/papers/a/PAPER.md",
            "/vault/papers/a/source/main.tex",
            "/vault/NOTES.txt",
            "/vault/data.bib",
        ] {
            assert_eq!(
                cache_control_for(Path::new(sidecar)),
                HeaderValue::from_static("private, no-cache"),
                "{sidecar} must revalidate after an editor write"
            );
        }
    }

    #[test]
    fn etag_matches_handles_lists_weak_tags_and_star() {
        let etag = "\"17a2b-10\"";
        assert!(etag_matches(etag, etag));
        assert!(etag_matches(&format!("W/{etag}"), etag));
        assert!(etag_matches(&format!("\"other\", {etag}"), etag));
        assert!(etag_matches("*", etag));
        assert!(!etag_matches("\"other\"", etag));
        assert!(!etag_matches("\"17a2b-11\"", etag));
    }

    #[test]
    fn etag_changes_when_size_or_mtime_changes() {
        let dir = std::env::temp_dir().join(format!(
            "agentero-web-etag-{}-{:?}",
            std::process::id(),
            SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.bin");
        std::fs::write(&file, b"one").unwrap();
        let first = file_etag(&std::fs::metadata(&file).unwrap()).unwrap();
        std::fs::write(&file, b"two-two").unwrap();
        let second = file_etag(&std::fs::metadata(&file).unwrap()).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with('"') && first.ends_with('"'));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn if_modified_since_compares_at_second_granularity() {
        let modified = UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_999);
        let same_second = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let older = UNIX_EPOCH + std::time::Duration::from_secs(1_699_999_999);
        assert!(not_modified_since(modified, same_second));
        assert!(not_modified_since(modified, older + std::time::Duration::from_secs(1)));
        assert!(!not_modified_since(modified, older));
    }

    #[test]
    fn parses_single_byte_ranges() {
        assert_eq!(parse_range_header("bytes=0-99", 1_000), ByteRange::Partial(0, 99));
        assert_eq!(
            parse_range_header("bytes=900-", 1_000),
            ByteRange::Partial(900, 999),
            "open-ended range runs to EOF"
        );
        assert_eq!(
            parse_range_header("bytes=-100", 1_000),
            ByteRange::Partial(900, 999),
            "suffix range"
        );
        assert_eq!(
            parse_range_header("bytes=990-2000", 1_000),
            ByteRange::Partial(990, 999),
            "end clamps to EOF"
        );
        assert_eq!(
            parse_range_header("bytes=0-0", 1_000),
            ByteRange::Partial(0, 0)
        );
        assert_eq!(
            parse_range_header("bytes=0-99, 200-299", 1_000),
            ByteRange::Ignore,
            "multi-range falls back to 200"
        );
        assert_eq!(parse_range_header("items=0-1", 1_000), ByteRange::Ignore);
        assert_eq!(parse_range_header("bytes=abc", 1_000), ByteRange::Ignore);
        assert_eq!(
            parse_range_header("bytes=1000-", 1_000),
            ByteRange::Unsatisfiable
        );
        assert_eq!(
            parse_range_header("bytes=5-2", 1_000),
            ByteRange::Unsatisfiable
        );
        assert_eq!(
            parse_range_header("bytes=-0", 1_000),
            ByteRange::Unsatisfiable
        );
        assert_eq!(parse_range_header("bytes=0-10", 0), ByteRange::Unsatisfiable);
    }
}
