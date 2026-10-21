//! Built-in Agentero provider — credentials baked in at build time.
//!
//! Release/CI builds inject the `AGENTERO_BUILTIN_*` environment variables at
//! compile time; the base URL and the three model ids have fallbacks, the API
//! key does not. A build without `AGENTERO_BUILTIN_API_KEY` therefore reports
//! the provider as unavailable and the UI hides it, rather than offering a
//! provider that cannot authenticate.
//!
//! The key never leaves the Host process: [`builtin_provider_status`] returns
//! only non-secret fields, and nothing here is ever written into
//! `AppSettings`, so the key cannot reach `settings.json` or the webview.

use serde::Serialize;

/// Fallback gateway. Not a secret: it ships inside every client binary and can
/// be enforced against (model allowlist, rate limit, spend cap) server-side.
const DEFAULT_BASE_URL: &str = "https://api.qiyuanchen.top/v1";
const DEFAULT_TRANSLATE_MODEL: &str = "tencent/Hunyuan-MT-7B";
const DEFAULT_EMBEDDING_MODEL: &str = "BAAI/bge-m3";
const DEFAULT_OCR_MODEL: &str = "PaddlePaddle/PaddleOCR-VL-1.5";

/// Compile-time value with a fallback; whitespace-only counts as unset.
fn injected(value: Option<&'static str>, fallback: &'static str) -> &'static str {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => value,
        None => fallback,
    }
}

fn base_url() -> &'static str {
    injected(option_env!("AGENTERO_BUILTIN_BASE_URL"), DEFAULT_BASE_URL)
}

fn translate_model() -> &'static str {
    injected(
        option_env!("AGENTERO_BUILTIN_TRANSLATE_MODEL"),
        DEFAULT_TRANSLATE_MODEL,
    )
}

fn embedding_model() -> &'static str {
    injected(
        option_env!("AGENTERO_BUILTIN_EMBEDDING_MODEL"),
        DEFAULT_EMBEDDING_MODEL,
    )
}

fn ocr_model() -> &'static str {
    injected(option_env!("AGENTERO_BUILTIN_OCR_MODEL"), DEFAULT_OCR_MODEL)
}

/// The only reader of the built-in key. `None` when it was not compiled in.
pub fn api_key() -> Option<&'static str> {
    option_env!("AGENTERO_BUILTIN_API_KEY")
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

pub fn available() -> bool {
    api_key().is_some()
}

/// Non-secret snapshot for Host-side consumers resolving built-in credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinProviderStatus {
    pub available: bool,
    pub base_url: String,
    pub translate_model: String,
    pub embedding_model: String,
    pub ocr_model: String,
}

pub fn status() -> BuiltinProviderStatus {
    BuiltinProviderStatus {
        available: available(),
        base_url: base_url().to_string(),
        translate_model: translate_model().to_string(),
        embedding_model: embedding_model().to_string(),
        ocr_model: ocr_model().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TRAP for maintainers: every `AGENTERO_BUILTIN_*` read above resolves via
    // `option_env!` at COMPILE time, so `std::env::set_var` at runtime changes
    // nothing. These tests assert the state of the build they run in, and guard
    // the key-present branches so a release build with the key injected still
    // passes.

    #[test]
    fn non_secret_values_fall_back_when_not_injected() {
        if option_env!("AGENTERO_BUILTIN_BASE_URL").is_none() {
            assert_eq!(base_url(), DEFAULT_BASE_URL);
        }
        if option_env!("AGENTERO_BUILTIN_TRANSLATE_MODEL").is_none() {
            assert_eq!(translate_model(), DEFAULT_TRANSLATE_MODEL);
        }
        if option_env!("AGENTERO_BUILTIN_EMBEDDING_MODEL").is_none() {
            assert_eq!(embedding_model(), DEFAULT_EMBEDDING_MODEL);
        }
        if option_env!("AGENTERO_BUILTIN_OCR_MODEL").is_none() {
            assert_eq!(ocr_model(), DEFAULT_OCR_MODEL);
        }
        assert!(!base_url().trim().is_empty());
        assert!(!translate_model().trim().is_empty());
        assert!(!embedding_model().trim().is_empty());
        assert!(!ocr_model().trim().is_empty());
    }

    #[test]
    fn availability_follows_the_compiled_in_key() {
        assert_eq!(available(), api_key().is_some());
        match option_env!("AGENTERO_BUILTIN_API_KEY") {
            // Presence is not enough: a whitespace-only value counts as unset.
            Some(raw) => assert_eq!(available(), !raw.trim().is_empty()),
            None => {
                assert!(api_key().is_none());
                assert!(!available());
            }
        }
    }

    #[test]
    fn status_carries_no_key_material() {
        let snapshot = status();
        let value = serde_json::to_value(&snapshot).expect("status serializes");
        let fields = value.as_object().expect("status is an object");

        let expected = [
            "available",
            "baseUrl",
            "translateModel",
            "embeddingModel",
            "ocrModel",
        ];
        assert_eq!(fields.len(), expected.len(), "unexpected status field");
        for name in expected {
            assert!(fields.contains_key(name), "missing {name}");
        }
        for banned in [
            "key",
            "apiKey",
            "api_key",
            "secret",
            "token",
            "auth",
            "credential",
            "mask",
        ] {
            assert!(!fields.contains_key(banned), "{banned} must not be exposed");
        }
        assert_eq!(fields["available"], snapshot.available);

        let encoded = value.to_string();
        if let Some(key) = option_env!("AGENTERO_BUILTIN_API_KEY") {
            assert!(!encoded.contains(key.trim()));
        }
    }
}
