//! Vault tag index RPC (`tags_index_read`).
//!
//! The catalog is authoritative; the hidden `<vault>/.agentero/tag-index.json`
//! cache and its lazy rebuild logic live in
//! `agentero_core::features::paper::catalog::tag_index` so the Web host, the
//! desktop Host and the CLI share one implementation.
//!
//! Wire shape (`ApiResult` envelope, camelCase):
//! `{ generatedAt, tags: [{name,color,count}], papers: [{path,tags}],
//!    untagged: [path] }` — internal `@zotero:` / `@arxiv:` tags are filtered.

use serde::Deserialize;
use serde_json::Value;

use agentero_core::features::paper::catalog::tag_index;

use crate::error::{blocking, fail_msg, ok, parse_args, vault_root, HandlerResult};
use crate::rpc::RpcCtx;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TagsIndexArgs {
    vault_path: String,
}

/// `tags_index_read({vaultPath})` → `ApiResult<TagIndexView>`.
pub async fn read(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let _ = ctx;
    let args: TagsIndexArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        tag_index::read_tag_index(&vault).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(view) => ok(view),
        Err(error) => fail_msg(error),
    }
}
