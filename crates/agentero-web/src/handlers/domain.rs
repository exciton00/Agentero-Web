//! Domain handler registry.
//!
//! Feature work appends `command => handler` arms here so the dispatch table in
//! `rpc.rs` (owned by the platform layer) stays untouched. Return `None` for
//! commands this module does not own; `rpc.rs` then falls back to the platform
//! commands and finally to the stub table.

use serde_json::Value;

use crate::error::HandlerResult;
use crate::handlers::{agent, layout, markdown, paper, search, tags, vault};
use crate::rpc::RpcCtx;

pub async fn dispatch(ctx: &RpcCtx, command: &str, args: &Value) -> Option<HandlerResult> {
    // Agent (BYOA/ACP) + doctor surface has its own registry so this table stays
    // readable; it claims `agent_*` / `doctor_*` / `remote_agent_*` names.
    if let Some(result) = agent::dispatch(ctx, command, args).await {
        return Some(result);
    }
    let result = match command {
        // ---- Vault file search ----
        "vault_search" => search::vault_search(ctx, args).await,

        // ---- Vault tag index (sidebar "by tag" tree + Library tag filter) ----
        "tags_index_read" => tags::read(ctx, args).await,

        // ---- Remote PDF layout analysis (MinerU / Paddle / OpenAI-compatible) ----
        "layout_remote_analyze_pdf" => layout::analyze_pdf(ctx, args).await,
        "layout_remote_probe" => layout::probe(ctx, args).await,

        // ---- Paper catalog mutations + detail bundles ----
        "paper_open_bundle" => paper::open_bundle(ctx, args).await,
        "paper_assets_status" => paper::assets_status(ctx, args).await,
        "paper_delete_assets" => paper::delete_assets(ctx, args).await,
        "paper_set_tags" => paper::set_tags(ctx, args).await,
        "paper_set_is_read" => paper::set_is_read(ctx, args).await,
        "paper_update_meta" => paper::update_meta(ctx, args).await,
        "paper_move" => paper::move_paper(ctx, args).await,
        "paper_repath" => paper::repath(ctx, args).await,
        "paper_rescan" => paper::rescan(ctx, args).await,
        "paper_set_page_counts" => paper::set_page_counts(ctx, args).await,
        "paper_refs_list" => paper::refs_list(ctx, args).await,
        "paper_refs_parse" => paper::refs_parse(ctx, args).await,
        "paper_sync_publication" => paper::sync_publication(ctx, args).await,
        "notes_template_seed" => paper::notes_template_seed(ctx, args).await,
        "activity_record_events" => paper::activity_record_events(ctx, args).await,

        // ---- Recycle bin + desktop-only path actions ----
        "path_trash" => vault::path_trash(ctx, args).await,
        "path_list_trash" => vault::path_list_trash(ctx, args).await,
        "path_restore_item" => vault::path_restore_item(ctx, args).await,
        "path_purge_item" => vault::path_purge_item(ctx, args).await,
        "path_purge_trash" => vault::path_purge_trash(ctx, args).await,
        "path_open_in_terminal" => vault::open_in_terminal(ctx, args).await,

        // ---- Vault session holds (process-wide on the server: no-ops) ----
        "vault_hold" => vault::hold(ctx, args).await,
        "vault_release" => vault::release(ctx, args).await,

        // ---- Markdown / wiki index reads + link-aware writes ----
        "graph_get_backlinks" => markdown::graph_get_backlinks(ctx, args).await,
        "graph_rebuild" => markdown::graph_rebuild(ctx, args).await,
        "wiki_resolve" => markdown::wiki_resolve(ctx, args).await,
        "wiki_embed_read" => markdown::wiki_embed_read(ctx, args).await,
        "wiki_search" => markdown::wiki_search(ctx, args).await,
        "wiki_move" => markdown::wiki_move(ctx, args).await,
        "wiki_rename_heading" => markdown::wiki_rename_heading(ctx, args).await,
        "wiki_external_rename_preview" => markdown::wiki_external_rename_preview(ctx, args).await,
        "wiki_apply_external_rename_repair" => {
            markdown::wiki_apply_external_rename_repair(ctx, args).await
        }

        _ => return None,
    };
    Some(result)
}
