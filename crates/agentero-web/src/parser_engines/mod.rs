//! Desktop cloud body-parse engines (MinerU / Paddle / OpenAI-compatible VLM,
//! the latter also serving the built-in provider).
//!
//! These implement agentero-core's [`engines::BodyParseEngine`] trait and produce the
//! markdown written to PAPER.md. They register into the core dynamic engine
//! registry via [`register_body_engines`], called from [`refresh_parser_config`]
//! at startup and on every settings change.
//!
//! # Relationship to `layout::hosted`
//!
//! The MinerU and Paddle engines here are thin markdown-extraction layers over
//! the cloud job runners in [`crate::features::paper::analyze::layout::hosted`] (upload →
//! poll → zip/JSONL). Sharing those runners avoids duplicating the HTTP
//! orchestration; the same cloud job is run once and consumed differently
//! (zip → `full.md` plus `images/` assets here; zip → `content_list.json` boxes there).
//! `openai_vlm` is self-contained and does not touch `layout::hosted`.
//!
//! Both trees draw credentials from the single `layout.providerConfigs`
//! settings pool via the `layout_*` accessors — the naming is historical, the
//! sharing is intentional. `openaiCompatible` is deliberately split:
//! `layout::hosted` holds a probe-only stub (connectivity check for the
//! settings UI), while the full VLM OCR engine lives here.
//!
//! The tauri-free engine framework (trait, snapshot, dispatch, local liteparse
//! engine) lives in `agentero_core::features::paper::analyze::parse::engines`.

pub(crate) mod mineru;
pub(crate) mod openai_vlm;
pub(crate) mod paddle;

use agentero_core::features::paper::analyze::parse::engines;
use agentero_core::features::system::settings::{AppSettingsStore, BUILTIN_PROVIDER_ID};
use std::collections::HashMap;
use std::sync::Arc;

/// Providers whose credentials are snapshotted into the parser registry.
const CREDENTIAL_PROVIDERS: &[&str] =
    &["paddle", "mineru", "openaiCompatible", BUILTIN_PROVIDER_ID];

/// Register the cloud engines and the settings-backed provider resolver with
/// the core engine registry. Idempotent (replaces existing entries).
pub fn register_body_engines() {
    engines::register_engine("mineru", || Arc::new(mineru::MineruBodyEngine));
    engines::register_engine("paddle", || Arc::new(paddle::PaddleBodyEngine));
    engines::register_engine("openaicompatible", || {
        Arc::new(openai_vlm::OpenAiVlmBodyEngine::new("openaiCompatible"))
    });
    // The built-in provider is the same VLM engine against a build-time
    // gateway; carrying its own id keeps failure notes naming the backend the
    // user selected.
    engines::register_engine(BUILTIN_PROVIDER_ID, || {
        Arc::new(openai_vlm::OpenAiVlmBodyEngine::new(BUILTIN_PROVIDER_ID))
    });
    engines::set_provider_resolver(agentero_core::features::system::settings::layout_provider_settings_key);
}

/// Rebuild the snapshot from the settings store; plaintext keys never leave
/// the Host process.
pub fn refresh_parser_config(store: &AppSettingsStore) {
    register_body_engines();
    engines::configure_parser(engines::ParserEngineConfig {
        backend: store.parser_backend(),
        credentials: parser_credentials(store),
    });
}

/// Credential snapshot per provider, keyed by the id `provider_for_backend`
/// returns. The built-in entry resolves from the build, not from a settings
/// card, so it can never be persisted.
fn parser_credentials(store: &AppSettingsStore) -> HashMap<String, engines::EngineCredentials> {
    let mut credentials = HashMap::new();
    for provider in CREDENTIAL_PROVIDERS {
        credentials.insert(
            (*provider).to_string(),
            engines::EngineCredentials {
                api_key: store.layout_api_key(provider),
                base_url: store.layout_base_url(provider),
                model: store.layout_model(provider),
                prompt: store.layout_prompt(provider),
                language: store.layout_language(provider),
                is_ocr: store.layout_is_ocr(provider),
                self_hosted: store.layout_self_hosted(provider),
            },
        );
    }
    credentials
}


/// Headless-host entry point: register cloud engines and snapshot the current
/// `layout.parser_backend` credentials. Called at startup and after settings_set.
pub fn refresh_from_store(store: &AppSettingsStore) {
    register_body_engines();
    agentero_core::features::paper::analyze::parse::engines::configure_parser(
        agentero_core::features::paper::analyze::parse::engines::ParserEngineConfig {
            backend: store.parser_backend(),
            credentials: parser_credentials(store),
        },
    );
}
