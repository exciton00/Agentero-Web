//! WeKnora REST client.
//!
//! All external HTTP goes through [`crate::http::client_with`] (configured
//! proxy, redirect cap, timeout). The API key travels in the `X-API-Key`
//! header and is never logged or echoed in error messages.
//!
//! Response envelope (WeKnora v0.8.x):
//! - success: `{"success":true,"data":…}`
//! - error: `{"error":{"code","message","details"},"success":false}`.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::features::rag::settings::RagSettings;
use crate::http::{client_with, DEFAULT_REDIRECT_LIMIT};

/// WeKnora's REST prefix; the configured base URL may already include it.
const API_PREFIX: &str = "/api/v1";
/// WeKnora limits `custom_metadata` to 20 fields with values ≤1000 chars.
pub const MAX_CUSTOM_METADATA_FIELDS: usize = 20;
pub const MAX_CUSTOM_METADATA_VALUE_CHARS: usize = 1_000;
/// Truncate upstream error text so a broken server cannot flood logs/UI.
const ERROR_SNIPPET_CHARS: usize = 400;

/// Stable error classification (drives UI messages and sync retry policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeKnoraErrorKind {
    NotConfigured,
    InvalidBaseUrl,
    Auth,
    Forbidden,
    NotFound,
    RateLimited,
    Timeout,
    Transport,
    /// Reserved provider/integration path is not implemented yet.
    NotImplemented,
    Api,
    Malformed,
    Metadata,
}

/// One client failure. `Display` is safe to show in the settings page / logs:
/// it never contains the API key or request headers.
#[derive(Debug, Clone)]
pub struct WeKnoraError {
    pub kind: WeKnoraErrorKind,
    pub status: Option<u16>,
    /// Upstream machine code (e.g. `1006`, `invalid_api_key`), when provided.
    pub code: Option<String>,
    pub message: String,
}

impl WeKnoraError {
    pub fn new(kind: WeKnoraErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            code: None,
            message: message.into(),
        }
    }

    pub fn is_auth(&self) -> bool {
        matches!(
            self.kind,
            WeKnoraErrorKind::Auth | WeKnoraErrorKind::Forbidden
        )
    }

    /// Retryable transport/rate-limit failure (sync can retry a document).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            WeKnoraErrorKind::RateLimited | WeKnoraErrorKind::Timeout | WeKnoraErrorKind::Transport
        )
    }

    /// Stable `AppError` code for the web/CLI wire.
    pub fn code(&self) -> &'static str {
        match self.kind {
            WeKnoraErrorKind::NotConfigured => "weknora_not_configured",
            WeKnoraErrorKind::InvalidBaseUrl => "weknora_invalid_base_url",
            WeKnoraErrorKind::Auth => "weknora_auth",
            WeKnoraErrorKind::Forbidden => "weknora_forbidden",
            WeKnoraErrorKind::NotFound => "weknora_not_found",
            WeKnoraErrorKind::RateLimited => "weknora_rate_limited",
            WeKnoraErrorKind::Timeout => "weknora_timeout",
            WeKnoraErrorKind::Transport => "weknora_transport",
            WeKnoraErrorKind::NotImplemented => "rag_provider_not_implemented",
            WeKnoraErrorKind::Api => "weknora_api",
            WeKnoraErrorKind::Malformed => "weknora_malformed",
            WeKnoraErrorKind::Metadata => "weknora_metadata_invalid",
        }
    }
}

impl std::fmt::Display for WeKnoraError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.code, self.status) {
            (Some(code), Some(status)) => write!(
                formatter,
                "{} (HTTP {status}, code {code}): {}",
                self.kind_label(),
                self.message
            ),
            (Some(code), None) => write!(
                formatter,
                "{} (code {code}): {}",
                self.kind_label(),
                self.message
            ),
            (None, Some(status)) => {
                write!(formatter, "{} (HTTP {status}): {}", self.kind_label(), self.message)
            }
            (None, None) => write!(formatter, "{}: {}", self.kind_label(), self.message),
        }
    }
}

impl std::error::Error for WeKnoraError {}

impl WeKnoraError {
    fn kind_label(&self) -> &'static str {
        match self.kind {
            WeKnoraErrorKind::NotConfigured => "WeKnora is not configured",
            WeKnoraErrorKind::InvalidBaseUrl => "invalid WeKnora base URL",
            WeKnoraErrorKind::Auth => "WeKnora authentication failed",
            WeKnoraErrorKind::Forbidden => "WeKnora permission denied",
            WeKnoraErrorKind::NotFound => "WeKnora resource not found",
            WeKnoraErrorKind::RateLimited => "WeKnora rate limit",
            WeKnoraErrorKind::Timeout => "WeKnora request timed out",
            WeKnoraErrorKind::Transport => "cannot reach WeKnora",
            WeKnoraErrorKind::NotImplemented => "RAG provider not implemented",
            WeKnoraErrorKind::Api => "WeKnora API error",
            WeKnoraErrorKind::Malformed => "malformed WeKnora response",
            WeKnoraErrorKind::Metadata => "invalid WeKnora metadata",
        }
    }
}

impl From<WeKnoraError> for crate::error::AppError {
    fn from(error: WeKnoraError) -> Self {
        crate::error::AppError::domain(error.code(), error.to_string())
    }
}

/// Knowledge-base summary (`GET /knowledge-bases`, `GET /knowledge-bases/:id`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KnowledgeBaseInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub knowledge_count: Option<u64>,
    pub embedding_model_id: Option<String>,
    pub capabilities: Option<Value>,
}

/// One WeKnora knowledge document (manual Markdown).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KnowledgeInfo {
    pub id: String,
    pub title: Option<String>,
    /// `pending` | `processing` | `finalizing` | `completed` | `failed` | …
    pub parse_status: Option<String>,
    /// `enabled` when the document is searchable.
    pub enable_status: Option<String>,
    pub custom_metadata: Option<Map<String, Value>>,
    pub tags: Vec<TagInfo>,
    pub updated_at: Option<String>,
}

impl KnowledgeInfo {
    /// Indexing has settled: the document can be searched.
    pub fn is_indexed(&self) -> bool {
        let enabled = self
            .enable_status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case("enabled"));
        let parsed = self
            .parse_status
            .as_deref()
            .is_some_and(|status| {
                status.eq_ignore_ascii_case("completed")
                    || status.eq_ignore_ascii_case("finalizing")
            });
        enabled && parsed
    }

    pub fn is_failed(&self) -> bool {
        self.parse_status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case("failed"))
    }
}

/// Knowledge-base tag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TagInfo {
    pub id: String,
    pub name: String,
}

/// Retrieval mode (maps to WeKnora's `disable_*` switches).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    #[default]
    Hybrid,
    Semantic,
    Keyword,
}

impl SearchMode {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "semantic" | "vector" => Self::Semantic,
            "keyword" | "bm25" => Self::Keyword,
            _ => Self::Hybrid,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Semantic => "semantic",
            Self::Keyword => "keyword",
        }
    }
}

/// One retrieval hit.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SearchHit {
    pub id: String,
    pub content: String,
    pub knowledge_id: String,
    pub knowledge_title: Option<String>,
    /// `summary` | `text` | …
    pub chunk_type: Option<String>,
    pub score: Option<f64>,
}

/// `POST /knowledge-search` request.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub knowledge_base_ids: Vec<String>,
    /// Restrict to specific documents (single-paper search).
    pub knowledge_ids: Vec<String>,
    pub tag_ids: Vec<String>,
    pub match_count: usize,
    pub mode: SearchMode,
    pub rerank: bool,
}

impl SearchRequest {
    fn body(&self) -> Value {
        let mut body = serde_json::json!({
            "query": self.query,
            "knowledge_base_ids": self.knowledge_base_ids,
            "match_count": self.match_count,
            "disable_keywords_match": self.mode == SearchMode::Semantic,
            "disable_vector_match": self.mode == SearchMode::Keyword,
            "rerank": { "enabled": self.rerank },
        });
        if !self.knowledge_ids.is_empty() {
            body["knowledge_ids"] = serde_json::json!(self.knowledge_ids);
        }
        if !self.tag_ids.is_empty() {
            body["tag_ids"] = serde_json::json!(self.tag_ids);
        }
        body
    }
}

/// Normalize the configured base URL into `<origin>/api/v1`.
///
/// Accepts `https://host`, `https://host/`, `https://host/api/v1` and
/// `https://host/api/v1/`. Anything that is not http(s) is rejected.
pub fn normalize_base_url(raw: &str) -> Result<String, WeKnoraError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(WeKnoraError::new(
            WeKnoraErrorKind::NotConfigured,
            "WeKnora base URL is empty — fill it in Settings → RAG",
        ));
    }
    let parsed = reqwest::Url::parse(raw).map_err(|error| {
        WeKnoraError::new(
            WeKnoraErrorKind::InvalidBaseUrl,
            format!("{raw}: {error}"),
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(WeKnoraError::new(
            WeKnoraErrorKind::InvalidBaseUrl,
            format!("base URL must use http(s), got {}", parsed.scheme()),
        ));
    }
    if parsed.host_str().is_none() {
        return Err(WeKnoraError::new(
            WeKnoraErrorKind::InvalidBaseUrl,
            "base URL has no host",
        ));
    }
    let path = parsed.path().trim_end_matches('/');
    let path = if path.is_empty() {
        API_PREFIX.to_string()
    } else if path.ends_with(API_PREFIX) {
        path.to_string()
    } else {
        format!("{path}{API_PREFIX}")
    };
    Ok(format!("{}{path}", parsed.origin().ascii_serialization()))
}

/// WeKnora REST client bound to one base URL + API key.
#[derive(Clone)]
pub struct WeKnoraClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl std::fmt::Debug for WeKnoraClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WeKnoraClient")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .finish()
    }
}

impl WeKnoraClient {
    /// Build a client from settings. The knowledge-base id is not part of the
    /// client; calls pass it explicitly.
    pub fn from_settings(settings: &RagSettings) -> Result<Self, WeKnoraError> {
        let base_url = normalize_base_url(settings.base_url())?;
        let timeout = Duration::from_millis(settings.retrieval.timeout_ms.max(1_000));
        Self::build(base_url, settings.api_key().map(str::to_string), timeout)
    }

    fn build(
        base_url: String,
        api_key: Option<String>,
        timeout: Duration,
    ) -> Result<Self, WeKnoraError> {
        let http = client_with(timeout, DEFAULT_REDIRECT_LIMIT, client_user_agent())
            .map_err(|error| WeKnoraError::new(WeKnoraErrorKind::Transport, error.to_string()))?;
        Ok(Self {
            http,
            base_url,
            api_key,
        })
    }

    /// Same credentials with a different request timeout (retrieval uses the
    /// shorter retrieval timeout; sync calls can use a longer one).
    pub fn with_timeout(&self, timeout: Duration) -> Result<Self, WeKnoraError> {
        Self::build(self.base_url.clone(), self.api_key.clone(), timeout)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // ---- knowledge bases --------------------------------------------------

    pub async fn list_knowledge_bases(
        &self,
    ) -> Result<Vec<KnowledgeBaseInfo>, WeKnoraError> {
        let data = self
            .request(reqwest::Method::GET, "/knowledge-bases", &[], None)
            .await?;
        Ok(parse_array(&data)
            .into_iter()
            .map(|value| parse_knowledge_base(&value))
            .collect())
    }

    pub async fn get_knowledge_base(
        &self,
        knowledge_base_id: &str,
    ) -> Result<KnowledgeBaseInfo, WeKnoraError> {
        let path = format!("/knowledge-bases/{}", encode_segment(knowledge_base_id));
        let data = self
            .request(reqwest::Method::GET, &path, &[], None)
            .await?;
        Ok(parse_knowledge_base(&data))
    }

    // ---- manual knowledge -------------------------------------------------

    /// Create one manual Markdown knowledge; returns the knowledge id.
    pub async fn create_manual(
        &self,
        knowledge_base_id: &str,
        title: &str,
        content: &str,
        tag_ids: &[String],
    ) -> Result<String, WeKnoraError> {
        let path = format!(
            "/knowledge-bases/{}/knowledge/manual",
            encode_segment(knowledge_base_id)
        );
        let body = serde_json::json!({
            "title": title,
            "content": content,
            "status": "publish",
            "tag_ids": tag_ids,
        });
        let data = self
            .request(reqwest::Method::POST, &path, &[], Some(body))
            .await?;
        extract_knowledge_id(&data).ok_or_else(|| {
            WeKnoraError::new(
                WeKnoraErrorKind::Malformed,
                "create manual knowledge response has no knowledge id",
            )
        })
    }

    /// Replace the whole Markdown body in place (version increments, re-index).
    pub async fn update_manual(
        &self,
        knowledge_id: &str,
        title: &str,
        content: &str,
        tag_ids: &[String],
    ) -> Result<(), WeKnoraError> {
        let path = format!("/knowledge/manual/{}", encode_segment(knowledge_id));
        let body = serde_json::json!({
            "title": title,
            "content": content,
            "status": "publish",
            "tag_ids": tag_ids,
        });
        self.request(reqwest::Method::PUT, &path, &[], Some(body))
            .await?;
        Ok(())
    }

    /// Overwrite the document title + `custom_metadata`.
    pub async fn update_metadata(
        &self,
        knowledge_id: &str,
        title: &str,
        custom_metadata: &Map<String, Value>,
    ) -> Result<(), WeKnoraError> {
        validate_custom_metadata(custom_metadata)?;
        let path = format!("/knowledge/{}", encode_segment(knowledge_id));
        let body = serde_json::json!({
            "title": title,
            "custom_metadata": Value::Object(custom_metadata.clone()),
        });
        self.request(reqwest::Method::PUT, &path, &[], Some(body))
            .await?;
        Ok(())
    }

    pub async fn get_knowledge(
        &self,
        knowledge_id: &str,
    ) -> Result<KnowledgeInfo, WeKnoraError> {
        let path = format!("/knowledge/{}", encode_segment(knowledge_id));
        let data = self
            .request(reqwest::Method::GET, &path, &[], None)
            .await?;
        Ok(parse_knowledge_info(&unwrap_knowledge(&data)))
    }

    /// List manual knowledge documents (one page).
    pub async fn list_knowledge(
        &self,
        knowledge_base_id: &str,
        page: u32,
        page_size: u32,
    ) -> Result<Vec<KnowledgeInfo>, WeKnoraError> {
        let path = format!(
            "/knowledge-bases/{}/knowledge",
            encode_segment(knowledge_base_id)
        );
        let query = [
            ("page", page.max(1).to_string()),
            ("page_size", page_size.clamp(1, 200).to_string()),
            ("source", "manual".to_string()),
        ];
        let data = self
            .request(reqwest::Method::GET, &path, &query, None)
            .await?;
        Ok(parse_array(&data)
            .into_iter()
            .map(|value| parse_knowledge_info(&unwrap_knowledge(&value)))
            .collect())
    }

    pub async fn delete_knowledge(&self, knowledge_id: &str) -> Result<(), WeKnoraError> {
        let path = format!("/knowledge/{}", encode_segment(knowledge_id));
        self.request(reqwest::Method::DELETE, &path, &[], None)
            .await?;
        Ok(())
    }

    /// Batch delete (≤200 ids per call at the upstream limit).
    pub async fn batch_delete_knowledge(
        &self,
        knowledge_base_id: &str,
        ids: &[String],
    ) -> Result<usize, WeKnoraError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let body = serde_json::json!({
            "kb_id": knowledge_base_id,
            "ids": ids,
        });
        self.request(reqwest::Method::POST, "/knowledge/batch-delete", &[], Some(body))
            .await?;
        Ok(ids.len())
    }

    // ---- tags -------------------------------------------------------------

    pub async fn list_tags(
        &self,
        knowledge_base_id: &str,
    ) -> Result<Vec<TagInfo>, WeKnoraError> {
        let path = format!(
            "/knowledge-bases/{}/tags",
            encode_segment(knowledge_base_id)
        );
        let data = self
            .request(reqwest::Method::GET, &path, &[], None)
            .await?;
        Ok(parse_array(&data).iter().map(parse_tag).collect())
    }

    pub async fn create_tag(
        &self,
        knowledge_base_id: &str,
        name: &str,
    ) -> Result<TagInfo, WeKnoraError> {
        let path = format!(
            "/knowledge-bases/{}/tags",
            encode_segment(knowledge_base_id)
        );
        let body = serde_json::json!({ "name": name });
        let data = self
            .request(reqwest::Method::POST, &path, &[], Some(body))
            .await?;
        let mut tag = parse_tag(&data);
        if tag.id.is_empty() {
            return Err(WeKnoraError::new(
                WeKnoraErrorKind::Malformed,
                "create tag response has no tag id",
            ));
        }
        if tag.name.is_empty() {
            tag.name = name.to_string();
        }
        Ok(tag)
    }

    /// Replace the tag set of every document in `updates` (whole map).
    pub async fn set_knowledge_tags(
        &self,
        knowledge_base_id: &str,
        updates: &HashMap<String, Vec<String>>,
    ) -> Result<(), WeKnoraError> {
        if updates.is_empty() {
            return Ok(());
        }
        let body = serde_json::json!({
            "kb_id": knowledge_base_id,
            "updates": updates,
        });
        self.request(reqwest::Method::PUT, "/knowledge/tags", &[], Some(body))
            .await?;
        Ok(())
    }

    // ---- retrieval --------------------------------------------------------

    pub async fn search(
        &self,
        request: &SearchRequest,
    ) -> Result<Vec<SearchHit>, WeKnoraError> {
        let data = self
            .request(
                reqwest::Method::POST,
                "/knowledge-search",
                &[],
                Some(request.body()),
            )
            .await?;
        Ok(parse_array(&data).iter().map(parse_search_hit).collect())
    }

    // ---- transport --------------------------------------------------------

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value, WeKnoraError> {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.http.request(method, &url);
        if let Some(api_key) = &self.api_key {
            request = request.header("X-API-Key", api_key);
        }
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(map_reqwest_error)?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            WeKnoraError::new(
                WeKnoraErrorKind::Transport,
                format!("read WeKnora response: {error}"),
            )
        })?;
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str::<Value>(&text).map_err(|error| {
                WeKnoraError::new(
                    WeKnoraErrorKind::Malformed,
                    format!(
                        "invalid WeKnora JSON ({}): {}",
                        error,
                        snippet(&text)
                    ),
                )
            })?
        };
        if status.is_success() {
            // Some endpoints answer `{success:false}` with HTTP 200.
            if value.get("success").and_then(Value::as_bool) == Some(false)
                || value.get("error").is_some_and(|error| !error.is_null())
            {
                return Err(api_error(status.as_u16(), &value));
            }
            Ok(value.get("data").cloned().unwrap_or(Value::Null))
        } else {
            Err(api_error(status.as_u16(), &value))
        }
    }
}

fn client_user_agent() -> &'static str {
    // The shared Host User-Agent names the product + version; keep it stable.
    crate::http::USER_AGENT
}

fn map_reqwest_error(error: reqwest::Error) -> WeKnoraError {
    if error.is_timeout() {
        WeKnoraError::new(
            WeKnoraErrorKind::Timeout,
            "the WeKnora request exceeded its timeout",
        )
    } else {
        WeKnoraError::new(
            WeKnoraErrorKind::Transport,
            format!("request to WeKnora failed: {error}"),
        )
    }
}

/// Map a non-2xx / envelope-level error to a classified error.
fn api_error(status: u16, value: &Value) -> WeKnoraError {
    let error = value.get("error").unwrap_or(value);
    let code = error
        .get("code")
        .map(value_to_compact_string)
        .filter(|code| !code.is_empty());
    let message = error
        .get("message")
        .or_else(|| error.get("msg"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("WeKnora returned HTTP {status}"));
    let kind = match status {
        401 => WeKnoraErrorKind::Auth,
        403 => WeKnoraErrorKind::Forbidden,
        404 => WeKnoraErrorKind::NotFound,
        429 => WeKnoraErrorKind::RateLimited,
        _ => WeKnoraErrorKind::Api,
    };
    WeKnoraError {
        kind,
        status: Some(status),
        code,
        message: snippet(&message),
    }
}

fn snippet(text: &str) -> String {
    let trimmed = text.trim();
    trimmed.chars().take(ERROR_SNIPPET_CHARS).collect()
}

/// Validate `custom_metadata` against the WeKnora limits before spending a call.
pub fn validate_custom_metadata(metadata: &Map<String, Value>) -> Result<(), WeKnoraError> {
    if metadata.len() > MAX_CUSTOM_METADATA_FIELDS {
        return Err(WeKnoraError::new(
            WeKnoraErrorKind::Metadata,
            format!(
                "custom_metadata has {} fields; WeKnora allows at most {MAX_CUSTOM_METADATA_FIELDS}",
                metadata.len()
            ),
        ));
    }
    for (key, value) in metadata {
        let length = match value {
            Value::String(text) => text.chars().count(),
            other => value_to_compact_string(other).chars().count(),
        };
        if length > MAX_CUSTOM_METADATA_VALUE_CHARS {
            return Err(WeKnoraError::new(
                WeKnoraErrorKind::Metadata,
                format!(
                    "custom_metadata field {key} is {length} chars; WeKnora allows at most {MAX_CUSTOM_METADATA_VALUE_CHARS}"
                ),
            ));
        }
    }
    Ok(())
}

fn value_to_compact_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn encode_segment(segment: &str) -> String {
    // Ids are opaque tokens; percent-encode anything outside the URL-safe set.
    urlencoding::encode(segment).into_owned()
}

/// Find the first JSON array: the value itself, `data.items`, `list`, …
fn parse_array(value: &Value) -> Vec<Value> {
    parse_array_depth(value, 0)
}

fn parse_array_depth(value: &Value, depth: usize) -> Vec<Value> {
    if depth > 3 {
        return Vec::new();
    }
    if let Some(array) = value.as_array() {
        return array.clone();
    }
    if let Some(object) = value.as_object() {
        for key in ["items", "list", "knowledge_list", "kbs", "tags", "results"] {
            if let Some(array) = object.get(key).and_then(Value::as_array) {
                return array.clone();
            }
        }
        if let Some(data) = object.get("data") {
            if data.is_array() || data.is_object() {
                return parse_array_depth(data, depth + 1);
            }
        }
    }
    Vec::new()
}

fn unwrap_knowledge(value: &Value) -> Value {
    if let Some(knowledge) = value.get("knowledge") {
        if knowledge.is_object() {
            return knowledge.clone();
        }
    }
    value.clone()
}

fn extract_knowledge_id(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        if !text.trim().is_empty() {
            return Some(text.trim().to_string());
        }
    }
    let object = value.as_object()?;
    for key in ["id", "knowledge_id", "knowledgeId"] {
        if let Some(text) = object.get(key).and_then(Value::as_str) {
            return Some(text.to_string());
        }
    }
    object
        .get("knowledge")
        .and_then(|inner| extract_knowledge_id(inner))
}

fn parse_knowledge_base(value: &Value) -> KnowledgeBaseInfo {
    let object = value.as_object();
    let get = |key: &str| object.and_then(|object| object.get(key));
    let string = |key: &str| {
        get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|text| !text.is_empty())
    };
    KnowledgeBaseInfo {
        id: string("id").or_else(|| string("kb_id")).unwrap_or_default(),
        name: string("name").unwrap_or_default(),
        description: string("description"),
        knowledge_count: get("knowledge_count").and_then(Value::as_u64),
        embedding_model_id: string("embedding_model_id"),
        capabilities: get("capabilities").cloned(),
    }
}

fn parse_knowledge_info(value: &Value) -> KnowledgeInfo {
    let object = value.as_object();
    let get = |key: &str| object.and_then(|object| object.get(key));
    let string = |key: &str| {
        get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|text| !text.is_empty())
    };
    let custom_metadata = get("custom_metadata")
        .and_then(Value::as_object)
        .cloned();
    let tags = get("tags")
        .and_then(Value::as_array)
        .map(|array| array.iter().map(parse_tag).collect())
        .unwrap_or_default();
    KnowledgeInfo {
        id: string("id")
            .or_else(|| string("knowledge_id"))
            .unwrap_or_default(),
        title: string("title"),
        parse_status: string("parse_status"),
        enable_status: string("enable_status"),
        custom_metadata,
        tags,
        updated_at: string("updated_at"),
    }
}

fn parse_tag(value: &Value) -> TagInfo {
    if let Some(text) = value.as_str() {
        return TagInfo {
            id: text.to_string(),
            name: String::new(),
        };
    }
    let object = value.as_object();
    let get = |key: &str| {
        object
            .and_then(|object| object.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default()
    };
    let nested = object
        .and_then(|object| object.get("tag").filter(|tag| tag.is_object()))
        .and_then(|tag| tag.as_object());
    let nested_get = |key: &str| {
        nested
            .and_then(|object| object.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default()
    };
    let id = get("id");
    let name = get("name");
    TagInfo {
        id: if id.is_empty() { nested_get("id") } else { id },
        name: if name.is_empty() {
            nested_get("name")
        } else {
            name
        },
    }
}

fn parse_search_hit(value: &Value) -> SearchHit {
    let object = value.as_object();
    let get = |key: &str| object.and_then(|object| object.get(key));
    SearchHit {
        id: get("id")
            .and_then(Value::as_str)
            .or_else(|| get("chunk_id").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string(),
        content: get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        knowledge_id: get("knowledge_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        knowledge_title: get("knowledge_title")
            .and_then(Value::as_str)
            .map(str::to_string),
        chunk_type: get("chunk_type").and_then(Value::as_str).map(str::to_string),
        score: get("score").and_then(|score| {
            score
                .as_f64()
                .or_else(|| score.as_str().and_then(|text| text.parse().ok()))
        }),
    }
}

/// JSON body used by tests / diagnostics: serialize a settings snapshot without
/// the key (never expose the secret through debug helpers).
pub fn redacted_settings_json(settings: &RagSettings) -> Value {
    let mut value = serde_json::to_value(settings).unwrap_or(Value::Null);
    if let Some(api_key) = value.get_mut("apiKey") {
        *api_key = Value::String(if settings.api_key().is_some() {
            "<set>".into()
        } else {
            String::new()
        });
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    struct MockResponse {
        status: u16,
        body: &'static str,
        delay: Duration,
    }

    fn mock_server(responses: Vec<MockResponse>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                // Read the request head + body (Content-Length) before responding.
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(read) => {
                            buffer.extend_from_slice(&chunk[..read]);
                            let text = String::from_utf8_lossy(&buffer);
                            if let Some(header_end) = text.find("\r\n\r\n") {
                                let content_length = text[..header_end]
                                    .lines()
                                    .find_map(|line| {
                                        line.to_ascii_lowercase()
                                            .strip_prefix("content-length:")
                                            .map(|value| value.trim().parse::<usize>().ok())
                                            .flatten()
                                    })
                                    .unwrap_or(0);
                                if buffer.len() >= header_end + 4 + content_length {
                                    break;
                                }
                            }
                        }
                        Err(_) => return,
                    }
                }
                if !response.delay.is_zero() {
                    thread::sleep(response.delay);
                }
                let reason = match response.status {
                    200 => "OK",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    404 => "Not Found",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    503 => "Service Unavailable",
                    _ => "Error",
                };
                let reply = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response.status,
                    reason,
                    response.body.as_bytes().len(),
                    response.body
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        url
    }

    fn response(status: u16, body: &'static str) -> MockResponse {
        MockResponse {
            status,
            body,
            delay: Duration::ZERO,
        }
    }

    fn client(url: &str) -> WeKnoraClient {
        let settings = RagSettings {
            base_url: url.to_string(),
            api_key: "test-key".into(),
            knowledge_base_id: "kb-1".into(),
            ..Default::default()
        };
        WeKnoraClient::from_settings(&settings).expect("client")
    }

    #[test]
    fn base_url_normalization_accepts_optional_api_prefix() {
        for (raw, expected) in [
            ("https://kb.example.com", "https://kb.example.com/api/v1"),
            ("https://kb.example.com/", "https://kb.example.com/api/v1"),
            (
                "https://kb.example.com/api/v1",
                "https://kb.example.com/api/v1",
            ),
            (
                "https://kb.example.com/api/v1/",
                "https://kb.example.com/api/v1",
            ),
            (
                "http://127.0.0.1:8080/weknora",
                "http://127.0.0.1:8080/weknora/api/v1",
            ),
        ] {
            assert_eq!(normalize_base_url(raw).unwrap(), expected, "{raw}");
        }
        assert!(normalize_base_url("").is_err());
        assert!(normalize_base_url("ftp://kb.example.com").is_err());
        assert!(normalize_base_url("not a url").is_err());
    }

    #[test]
    fn custom_metadata_limits_are_validated() {
        let mut metadata = Map::new();
        for index in 0..MAX_CUSTOM_METADATA_FIELDS {
            metadata.insert(format!("k{index}"), Value::String("v".into()));
        }
        assert!(validate_custom_metadata(&metadata).is_ok());
        metadata.insert("overflow".into(), Value::String("v".into()));
        let error = validate_custom_metadata(&metadata).unwrap_err();
        assert_eq!(error.kind, WeKnoraErrorKind::Metadata);

        let mut long = Map::new();
        long.insert(
            "k".into(),
            Value::String("x".repeat(MAX_CUSTOM_METADATA_VALUE_CHARS + 1)),
        );
        assert_eq!(
            validate_custom_metadata(&long).unwrap_err().kind,
            WeKnoraErrorKind::Metadata
        );
    }

    #[tokio::test]
    async fn create_manual_parses_nested_knowledge_id() {
        let url = mock_server(vec![response(
            200,
            r#"{"success":true,"data":{"id":"doc-123","knowledge_id":"doc-123"}}"#,
        )]);
        let client = client(&url);
        let id = client
            .create_manual("kb-1", "Title", "# body", &[])
            .await
            .expect("create");
        assert_eq!(id, "doc-123");
    }

    #[tokio::test]
    async fn error_statuses_map_to_typed_errors() {
        for (status, expected) in [
            (401, WeKnoraErrorKind::Auth),
            (403, WeKnoraErrorKind::Forbidden),
            (404, WeKnoraErrorKind::NotFound),
            (429, WeKnoraErrorKind::RateLimited),
            (500, WeKnoraErrorKind::Api),
        ] {
            let url = mock_server(vec![response(
                status,
                r#"{"success":false,"error":{"code":"x","message":"upstream says no"}}"#,
            )]);
            let client = client(&url);
            let error = client.get_knowledge("doc").await.unwrap_err();
            assert_eq!(error.kind, expected, "status {status}");
            assert_eq!(error.status, Some(status));
            assert!(!error.to_string().contains("test-key"), "{error}");
        }
    }

    #[tokio::test]
    async fn malformed_json_is_reported_without_echoing_the_key() {
        let url = mock_server(vec![response(200, "not json at all")]);
        let client = client(&url);
        let error = client.get_knowledge("doc").await.unwrap_err();
        assert_eq!(error.kind, WeKnoraErrorKind::Malformed);
        assert!(!error.to_string().contains("test-key"), "{error}");
    }

    #[tokio::test]
    async fn slow_server_times_out() {
        let url = mock_server(vec![MockResponse {
            status: 200,
            body: r#"{"success":true,"data":{"id":"doc"}}"#,
            delay: Duration::from_millis(400),
        }]);
        let client = client(&url).with_timeout(Duration::from_millis(80)).unwrap();
        let error = client.get_knowledge("doc").await.unwrap_err();
        assert_eq!(error.kind, WeKnoraErrorKind::Timeout, "{error}");
    }

    #[tokio::test]
    async fn knowledge_list_and_details_parse_status_fields() {
        let url = mock_server(vec![
            response(
                200,
                r#"{"success":true,"data":{"items":[{"id":"doc-1","title":"Paper","parse_status":"completed","enable_status":"enabled","custom_metadata":{"agentero_managed":"true"},"tags":[{"id":"t1","name":"GaN"}]}],"total":1}}"#,
            ),
            response(
                200,
                r#"{"success":true,"data":{"knowledge":{"id":"doc-1","parse_status":"finalizing","enable_status":"enabled","custom_metadata":{}}}}"#,
            ),
        ]);
        let client = client(&url);
        let rows = client.list_knowledge("kb-1", 1, 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_indexed());
        assert_eq!(rows[0].tags[0].name, "GaN");
        let info = client.get_knowledge("doc-1").await.unwrap();
        assert!(info.is_indexed(), "finalizing counts as searchable");
    }

    #[tokio::test]
    async fn search_request_disables_rerank_and_parses_hits() {
        let url = mock_server(vec![response(
            200,
            r#"{"success":true,"data":[{"id":"c1","content":"hello","knowledge_id":"doc-1","knowledge_title":"Paper","chunk_type":"summary","score":0.82}]}"#,
        )]);
        let client = client(&url);
        let hits = client
            .search(&SearchRequest {
                query: "hello".into(),
                knowledge_base_ids: vec!["kb-1".into()],
                knowledge_ids: Vec::new(),
                tag_ids: Vec::new(),
                match_count: 8,
                mode: SearchMode::Hybrid,
                rerank: false,
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].knowledge_id, "doc-1");
        assert_eq!(hits[0].chunk_type.as_deref(), Some("summary"));
        assert_eq!(hits[0].score, Some(0.82));
    }

    #[tokio::test]
    async fn tags_crud_roundtrip() {
        let url = mock_server(vec![
            response(
                200,
                r#"{"success":true,"data":[{"id":"t1","name":"GaN"}]}"#,
            ),
            response(
                200,
                r#"{"success":true,"data":{"tag":{"id":"t2","name":"trapping"}}}"#,
            ),
            response(200, r#"{"success":true,"data":{}}"#),
        ]);
        let client = client(&url);
        let tags = client.list_tags("kb-1").await.unwrap();
        assert_eq!(tags, vec![TagInfo { id: "t1".into(), name: "GaN".into() }]);
        let created = client.create_tag("kb-1", "trapping").await.unwrap();
        assert_eq!(created.id, "t2");
        let mut updates = HashMap::new();
        updates.insert("doc-1".to_string(), vec!["t1".to_string()]);
        client.set_knowledge_tags("kb-1", &updates).await.unwrap();
    }

    #[test]
    fn debug_output_never_contains_the_api_key() {
        let settings = RagSettings {
            base_url: "https://kb.example.com".into(),
            api_key: "super-secret".into(),
            ..Default::default()
        };
        let client = WeKnoraClient::from_settings(&settings).unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("super-secret"), "{rendered}");
        let redacted = redacted_settings_json(&settings);
        assert_eq!(redacted["apiKey"], "<set>");
    }

    #[allow(dead_code)]
    fn assert_send_sync() {
        fn requires_send_sync<T: Send + Sync>() {}
        requires_send_sync::<Arc<WeKnoraClient>>();
    }
}
