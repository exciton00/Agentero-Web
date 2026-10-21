//! Paper catalog handlers (list / detail / page counts / reading activity).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use agentero_core::features::paper::catalog::papers::{self, PaperRecord};
use agentero_core::features::pdf::marks::activity;

use crate::error::{
    blocking, fail_msg, ok, parse_args, typed_err, typed_ok, vault_root, HandlerResult,
};
use crate::rpc::RpcCtx;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperListArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperGetArgs {
    vault_path: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperPageCountsArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaperReadingActivityBatchArgs {
    vault_path: String,
    paths: Vec<String>,
}

/// One library row: the catalog record flattened plus a local-PDF probe.
/// Wire-compatible with the desktop `PaperListRow` (`has_pdf` is snake_case).
#[derive(Debug, Serialize)]
struct PaperListRow {
    #[serde(flatten)]
    paper: PaperRecord,
    has_pdf: bool,
}

/// `paper_list({vaultPath})` → `Result<ApiResult<PaperListRow[]>, String>`.
pub async fn list(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperListArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let caps = ctx.state.caps.clone();
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let rows = papers::list_all_unique_by_id(&vault).map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(|paper| {
                let has_pdf =
                    !paper.path.is_empty() && caps.caps_for(&vault, &paper.path).has_pdf();
                PaperListRow { paper, has_pdf }
            })
            .collect::<Vec<_>>())
    })
    .await;
    match result {
        Ok(rows) => typed_ok(rows),
        Err(error) => typed_err(error),
    }
}

/// `paper_get({vaultPath, path?, id?})` → `ApiResult<PaperRecord>`.
pub async fn get(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperGetArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let record = match (args.path.as_deref(), args.id.as_deref()) {
            (Some(path), _) if !path.trim().is_empty() => papers::get_by_path(&vault, path),
            (_, Some(id)) if !id.trim().is_empty() => papers::get_by_id(&vault, id),
            _ => return Err("path or id is required".to_string()),
        }
        .map_err(|error| error.to_string())?;
        record.ok_or_else(|| "paper not found".to_string())
    })
    .await;
    match result {
        Ok(record) => ok(record),
        Err(error) => fail_msg(error),
    }
}

/// `paper_page_counts({vaultPath})` → `ApiResult<HashMap<String, i64>>`.
pub async fn page_counts(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperPageCountsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        papers::list_page_counts(&vault).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(counts) => ok(counts),
        Err(error) => fail_msg(error),
    }
}

/// `paper_reading_activity_batch({vaultPath, paths})` → activity heatmap points.
pub async fn reading_activity_batch(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PaperReadingActivityBatchArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let out: HashMap<String, Vec<activity::ReadingActivityPoint>> =
            activity::collect_reading_activity(&vault, &args.paths);
        Ok(out)
    })
    .await;
    match result {
        Ok(points) => ok(points),
        Err(error) => fail_msg(error),
    }
}
