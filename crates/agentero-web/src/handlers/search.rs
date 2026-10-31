//! Vault full-text search (command palette "contents" tier) and Library
//! content-domain search (NOTES / highlights / captions).
//!
//! The walk/ranking implementations live in `agentero_core`; these handlers
//! are only transport shells for `vault_search` and `paper_search_content`.

use serde_json::Value;

use agentero_core::features::markdown::search::{self, VaultSearchArgs};
use agentero_core::features::paper::search_content::{
    self as paper_content, PaperContentSearchArgs,
};

use crate::error::{blocking, fail_msg, ok, parse_args, HandlerResult};
use crate::rpc::RpcCtx;

/// `vault_search({vaultPath, query, limit?, year?, publication?, doi?, isRead?})`
/// → `ApiResult<VaultSearchResult>`.
pub async fn vault_search(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: VaultSearchArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result =
        blocking(move || search::vault_search(args).map_err(|error| error.to_string())).await;
    match result {
        Ok(result) => ok(result),
        Err(error) => fail_msg(error),
    }
}

/// `paper_search_content({vaultPath, query, domains})`
/// → `ApiResult<{paperPaths: string[]}>`.
///
/// Matches the query against per-paper user content domains (notes / marks /
/// captions) and returns only the matching catalog paper folders; the source
/// text never crosses the wire. Cached in the core crate, invalidated by
/// sidecar (mtime, size), so the 495-paper library does not reread files on
/// every keystroke.
pub async fn paper_search_content(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperContentSearchArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        paper_content::search_paper_content(&args).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(result) => ok(result),
        Err(error) => fail_msg(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_search_content_accepts_camel_case_wire_args() {
        let parsed: PaperContentSearchArgs = parse_args(&serde_json::json!({
            "vaultPath": "/vault",
            "query": "notes token",
            "domains": ["notes", "marks"]
        }))
        .expect("camelCase args");
        assert_eq!(parsed.vault_path, "/vault");
        assert_eq!(parsed.query, "notes token");
        assert_eq!(parsed.domains, vec!["notes", "marks"]);
    }

    #[test]
    fn paper_search_content_rejects_unknown_domain_with_usage_error() {
        let args: PaperContentSearchArgs = parse_args(&serde_json::json!({
            "vaultPath": tempdir(),
            "query": "x",
            "domains": ["body"]
        }))
        .expect("args");
        let error = paper_content::search_paper_content(&args).expect_err("unknown domain");
        assert_eq!(error.code(), "usage");
    }

    fn tempdir() -> String {
        // A vault is only validated after domains parse: any existing directory
        // works for the domain-rejection path.
        std::env::temp_dir().to_string_lossy().into_owned()
    }
}
