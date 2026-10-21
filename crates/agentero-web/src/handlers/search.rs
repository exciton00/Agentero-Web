//! Vault full-text search (command palette "contents" tier).
//!
//! The walk/ranking implementation lives in `agentero_core::features::markdown`
//! `::search` (shared with the desktop Host); this handler is only the transport
//! shell for `vault_search`.

use serde_json::Value;

use agentero_core::features::markdown::search::{self, VaultSearchArgs};

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
