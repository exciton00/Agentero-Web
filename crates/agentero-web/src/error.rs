//! Error and JSON-envelope helpers.
//!
//! The wire contract is the desktop IPC one (`docs/backend/api.md` §2.2):
//! plain commands answer `ApiResult<T>` (`{ok, data | error}`), while
//! `Result<ApiResult<T>, String>` commands reject at the transport level. The
//! HTTP layer mirrors that: `Ok(value)` is a 200 JSON body, `Err(message)` is
//! the transport-level rejection the browser shim turns back into a throw.

use serde::Serialize;
use serde_json::Value;

pub use agentero_core::error::{ApiResult, AppError};

pub type HandlerResult = Result<Value, String>;

/// Success envelope for a plain `ApiResult<T>` command.
pub fn ok<T: Serialize>(data: T) -> HandlerResult {
    let value = serde_json::to_value(ApiResult::ok(data))
        .map_err(|error| format!("serialize_failed: {error}"))?;
    Ok(value)
}

/// Success envelope for a typed command (`Result<ApiResult<T>, String>`).
pub fn typed_ok<T: Serialize>(data: T) -> HandlerResult {
    ok(data)
}

/// Business failure inside the envelope (HTTP 200): the frontend throws from
/// the helper, matching desktop behavior for plain `ApiResult` commands.
pub fn fail(error: AppError) -> HandlerResult {
    let value = serde_json::to_value(ApiResult::<()>::err(error))
        .map_err(|error| format!("serialize_failed: {error}"))?;
    Ok(value)
}

pub fn fail_msg(message: impl Into<String>) -> HandlerResult {
    fail(AppError::message(message))
}

/// Transport-level failure for a typed command (HTTP 500 + error string).
pub fn typed_err(message: impl Into<String>) -> HandlerResult {
    Err(message.into())
}

/// Run a blocking domain operation on the tokio blocking pool.
pub async fn blocking<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(error) => Err(format!("blocking_task_failed: {error}")),
    }
}

/// Parse a command's argument object, tolerating the desktop `{ args: … }`
/// wrapper used by collection-style commands.
pub fn parse_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, String> {
    if let Some(inner) = args.get("args") {
        if inner.is_object() {
            return serde_json::from_value(inner.clone())
                .map_err(|error| format!("invalid_args: {error}"));
        }
    }
    serde_json::from_value(args.clone()).map_err(|error| format!("invalid_args: {error}"))
}

/// Resolve a user-supplied Vault root, rejecting remote handles and bad paths.
pub fn vault_root(vault_path: &str) -> Result<std::path::PathBuf, String> {
    agentero_core::fs::resolve_vault(vault_path).map_err(|error| error.to_string())
}
