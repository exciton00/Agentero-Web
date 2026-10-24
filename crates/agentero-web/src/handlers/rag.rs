//! RAG RPCs (`rag_*`).
//!
//! Every command answers a plain `ApiResult<T>` envelope (`{ok, data|error}`).
//! Long-running sync is fire-and-forget: `rag_sync_start` returns
//! `{started,…}` immediately and progress streams through `rag:sync`.

use serde::Deserialize;
use serde_json::Value;

use crate::error::{fail_msg, ok, parse_args, HandlerResult};
use crate::rpc::RpcCtx;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestArgs {
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    knowledge_base_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListKbsArgs {
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncStartArgs {
    #[serde(default)]
    force: bool,
    /// Optional single paper (id or vault-relative path).
    #[serde(default)]
    paper: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileArgs {
    #[serde(default)]
    prune: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClearMappingArgs {
    /// Requires explicit confirmation in the UI: deletes managed documents.
    #[serde(default)]
    delete_docs: bool,
}

/// `rag_status` → status summary +
/// `{enabled, configured, baseUrl, knowledgeBaseId, kbName?, sync, autoSync, tools}`.
pub async fn status(ctx: &RpcCtx) -> HandlerResult {
    ok(ctx.state.rag.status())
}

/// `rag_test {baseUrl?, apiKey?, knowledgeBaseId?}`.
pub async fn test(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: TestArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    ok(ctx
        .state
        .rag
        .test_connection(args.base_url, args.api_key, args.knowledge_base_id)
        .await)
}

/// `rag_list_kbs {baseUrl?, apiKey?}` → `{kbs, error?}`.
pub async fn list_kbs(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ListKbsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    ok(ctx
        .state
        .rag
        .list_kbs(args.base_url, args.api_key)
        .await)
}

/// `rag_sync_start {force?, paper?}` → `{started,total,changed,skipped,deleted}`.
pub async fn sync_start(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SyncStartArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    match ctx.state.rag.start_sync(args.force, args.paper) {
        Ok(value) => ok(value),
        Err(error) => fail_msg(error),
    }
}

/// `rag_sync_cancel` → `{cancelled}`.
pub async fn sync_cancel(ctx: &RpcCtx) -> HandlerResult {
    ok(ctx.state.rag.cancel())
}

/// `rag_reconcile {prune?}` → `{adopted,missing,orphans,deleted}`.
pub async fn reconcile(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ReconcileArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    match ctx.state.rag.reconcile(args.prune).await {
        Ok(value) => ok(value),
        Err(error) => fail_msg(error),
    }
}

/// `rag_clear_mapping {deleteDocs?}` → `{cleared,deleted}`.
pub async fn clear_mapping(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ClearMappingArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    match ctx.state.rag.clear_mapping(args.delete_docs).await {
        Ok(value) => ok(value),
        Err(error) => fail_msg(error),
    }
}

/// `rag_search {query, topK?, paper?, tag?, mode?, rerank?}`.
pub async fn search(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: crate::rag::SearchArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    match ctx.state.rag.search(args).await {
        Ok(value) => ok(value),
        Err(error) => fail_msg(error),
    }
}
