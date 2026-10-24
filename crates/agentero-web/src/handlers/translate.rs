//! Translation RPC (`translate_text`).
//!
//! The desktop command resolves BYOK credentials and durable custom templates
//! before calling the shared `agentero-core` translation engine. The web host
//! owns the same settings store, so it mirrors that logic server-side.

use agentero_core::features::system::settings::is_translate_api_key_mask;
use agentero_core::features::translate::{self, TranslateTextArgs};
use serde_json::Value;

use crate::error::{fail, fail_msg, ok, parse_args, AppError, HandlerResult};
use crate::rpc::RpcCtx;

pub async fn translate_text(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let mut args: TranslateTextArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };

    let provider = args.provider.trim().to_ascii_lowercase();
    if provider == translate::BUILTIN_PROVIDER_ID {
        // The web build does not vendor the desktop built-in gateway/key.
        return fail(AppError::domain(
            translate::ERR_NO_BUILTIN_KEY,
            "Built-in translation is not available in the web build.",
        ));
    }

    // Commercial BYOK: the frontend may send a `*` mask or omit the key; the
    // host resolves the real stored key from settings.
    let needs_stored_key = args
        .api_key
        .as_deref()
        .map(|key| {
            let key = key.trim();
            key.is_empty() || is_translate_api_key_mask(key)
        })
        .unwrap_or(true);
    if needs_stored_key {
        if let Some(key) = ctx.state.settings.translate_api_key(&provider) {
            args.api_key = Some(key);
        } else if args
            .api_key
            .as_deref()
            .is_some_and(is_translate_api_key_mask)
        {
            args.api_key = None;
        }
    }

    if provider == "openaicompatible" {
        let (prompt, system, user) = ctx.state.settings.translate_custom_templates();
        if args
            .custom_prompt
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
            && !prompt.is_empty()
        {
            args.custom_prompt = Some(prompt);
        }
        if args
            .custom_system
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
            && !system.is_empty()
        {
            args.custom_system = Some(system);
        }
        if args
            .custom_user
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
            && !user.is_empty()
        {
            args.custom_user = Some(user);
        }
    }

    match translate::translate_text(args).await {
        Ok(result) => ok(serde_json::to_value(result).unwrap_or(Value::Null)),
        Err(error) => fail(error),
    }
}
