//! Vault lifecycle + tree handlers.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use agentero_core::features::paper::catalog::tag_index;
use agentero_core::features::vault::{self, trash, tree};

use crate::error::{
    blocking, fail_msg, ok, parse_args, typed_err, typed_ok, vault_root, HandlerResult,
};
use crate::rpc::RpcCtx;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultCreateArgs {
    path: String,
    #[serde(default)]
    locale: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultTreeArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultTreeChildrenArgs {
    vault_path: String,
    dir_path: String,
}

#[derive(Debug, Deserialize)]
struct AllowFsScopeArgs {
    path: String,
}

fn absolute_path_arg(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("path is required".to_string());
    }
    Ok(PathBuf::from(trimmed))
}

/// `vault_create(path, locale)` → `ApiResult<CreateVaultResult>`.
pub async fn create(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: VaultCreateArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let path = absolute_path_arg(&args.path)?;
        let locale = vault::resolve_vault_locale(args.locale.as_deref().unwrap_or(""));
        vault::create_vault(&path, locale).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(value) => ok(value),
        Err(error) => fail_msg(error),
    }
}

/// `vault_ensure(path, locale)` → `ApiResult<CreateVaultResult>`.
pub async fn ensure(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: VaultCreateArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let result = blocking(move || {
        let path = absolute_path_arg(&args.path)?;
        let locale = vault::resolve_vault_locale(args.locale.as_deref().unwrap_or(""));
        vault::ensure_vault(&path, locale).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(value) => ok(value),
        Err(error) => fail_msg(error),
    }
}

/// `vault_tree_build(vaultPath)` → `Result<ApiResult<Vec<VaultTreeNode>>, String>`.
pub async fn tree_build(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: VaultTreeArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let caps = ctx.state.caps.clone();
    let result = blocking(move || {
        let root = vault_root(&args.vault_path)?;
        Ok(tree::build_tree(&root, &caps))
    })
    .await;
    match result {
        Ok(nodes) => typed_ok(nodes),
        Err(error) => typed_err(error),
    }
}

/// `vault_tree_children(vaultPath, dirPath)` → one directory's children.
pub async fn tree_children(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: VaultTreeChildrenArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let caps = ctx.state.caps.clone();
    let result = blocking(move || {
        let root = vault_root(&args.vault_path)?;
        let mut dir = PathBuf::from(args.dir_path.trim());
        if dir.is_relative() {
            dir = root.join(dir);
        }
        tree::list_children(&root, &dir, &caps).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(nodes) => typed_ok(nodes),
        Err(error) => typed_err(error),
    }
}

/// `vault_allow_fs_scope(path)` — the web host has no Tauri fs scope, so this
/// only validates the path and succeeds.
pub async fn allow_fs_scope(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: AllowFsScopeArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    match absolute_path_arg(&args.path) {
        Ok(_) => ok(serde_json::Value::Null),
        Err(error) => fail_msg(error),
    }
}

/// `vault_open_take_pending` — no deep-link / CLI open requests on the server.
pub async fn take_pending_open(_ctx: &RpcCtx) -> HandlerResult {
    ok(Option::<String>::None)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathTrashArgs {
    vault_path: String,
    /// Vault-relative paths to move into the recycle bin.
    rels: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrashVaultArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrashItemArgs {
    vault_path: String,
    batch_id: String,
    /// Basename of the stored copy inside the batch (from `path_list_trash`).
    stored: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PathRestoreItemResult {
    /// Vault-relative path the item was restored to.
    rel: String,
}

#[derive(Debug, Deserialize)]
struct PathArg {
    #[serde(default)]
    path: String,
}

/// `path_trash({vaultPath, rels})` → `ApiResult<TrashResult>`.
///
/// The desktop shell additionally cancels running jobs for trashed papers; that
/// lives in the server task center (runtime layer), not here.
pub async fn path_trash(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PathTrashArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let result = trash::trash_paths(&vault, &args.rels).map_err(|error| error.to_string())?;
        // Trashed papers left the catalog → refresh the cached tag index.
        tag_index::invalidate_tag_index(&vault);
        tag_index::ensure_tag_index_best_effort(&vault);
        Ok(result)
    })
    .await;
    match result {
        Ok(result) => typed_ok(result),
        Err(error) => fail_msg(error),
    }
}

/// `path_list_trash({vaultPath})` → `ApiResult<TrashEntry[]>`.
pub async fn path_list_trash(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: TrashVaultArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        trash::list_trash(&vault).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(items) => typed_ok(items),
        Err(error) => fail_msg(error),
    }
}

/// `path_restore_item({vaultPath, batchId, stored})` →
/// `ApiResult<PathRestoreItemResult>`.
pub async fn path_restore_item(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: TrashItemArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        let rel = trash::restore_item(&vault, &args.batch_id, &args.stored)
            .map_err(|error| error.to_string())?;
        // Restored catalog rows change the tag inventory.
        tag_index::invalidate_tag_index(&vault);
        tag_index::ensure_tag_index_best_effort(&vault);
        Ok(PathRestoreItemResult { rel })
    })
    .await;
    match result {
        Ok(result) => typed_ok(result),
        Err(error) => fail_msg(error),
    }
}

/// `path_purge_item({vaultPath, batchId, stored})` → `ApiResult<null>`.
pub async fn path_purge_item(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: TrashItemArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        trash::purge_item(&vault, &args.batch_id, &args.stored).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(()) => typed_ok(()),
        Err(error) => fail_msg(error),
    }
}

/// `path_purge_trash({vaultPath})` → `ApiResult<null>`.
pub async fn path_purge_trash(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: TrashVaultArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let result = blocking(move || {
        let vault = vault_root(&args.vault_path)?;
        trash::purge_all(&vault).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(()) => typed_ok(()),
        Err(error) => fail_msg(error),
    }
}

/// `vault_hold(path)` — the Web host keeps no per-window vault hold table:
/// server resources are process-wide and released on shutdown. No-op success.
pub async fn hold(_ctx: &RpcCtx, _args: &Value) -> HandlerResult {
    ok(Value::Null)
}

/// `vault_release(path)` — paired with [`hold`]; nothing to release per window.
pub async fn release(_ctx: &RpcCtx, _args: &Value) -> HandlerResult {
    ok(Value::Null)
}

/// `path_open_in_terminal(path)` — desktop-only integration (Finder/terminal).
pub async fn open_in_terminal(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PathArg = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return fail_msg(error),
    };
    let _ = args.path;
    fail_msg("opening a terminal is not available in the headless web host")
}
