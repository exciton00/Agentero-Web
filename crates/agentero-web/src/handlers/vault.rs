//! Vault lifecycle + tree handlers.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use agentero_core::features::vault::{self, tree};

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
