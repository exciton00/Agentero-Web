//! Markdown / wiki handlers: index-backed reads and link-aware writes.
//!
//! Mirrors the desktop command shells in
//! `src-tauri/src/features/markdown/wiki/commands.rs` and
//! `src-tauri/src/features/vault/rename/commands.rs`, but calls the tauri-free
//! services in `agentero-core` directly. Every command keeps the exact wire
//! shape declared in `src/lib/core/bindings.ts` (camelCase args + `ApiResult`
//! envelope; transport-level rejection only when the blocking task dies).

use serde::Deserialize;
use serde_json::Value;

use agentero_core::error::{ApiResult, AppError};
use agentero_core::features::markdown::wiki::heading_rename::run_heading_rename_transaction;
use agentero_core::features::markdown::wiki::models::{
    InternalLinkSyntax, WikiRenameErrorCode, WikiSearchCandidateKind,
};
use agentero_core::features::paper::catalog::papers;
use agentero_core::features::vault::rename::{
    run_local_rename_transaction, run_prepared_external_rename_repair, WikiExternalRenamePreview,
    WikiRenameTransaction,
};
use agentero_core::usage;

use crate::error::{blocking, parse_args, typed_err, vault_root, HandlerResult};
use crate::handlers::wiki_state::{lock_wiki_index, repair_store};
use crate::rpc::RpcCtx;

/// A domain failure raised inside a blocking closure; `details` fills the
/// `error.details` field when the desktop command attaches structured data.
struct BusinessError {
    message: String,
    details: Option<Value>,
}

impl BusinessError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: None,
        }
    }

    fn with_details(message: impl Into<String>, details: Value) -> Self {
        Self {
            message: message.into(),
            details: Some(details),
        }
    }

    fn into_api<T: serde::Serialize>(self) -> ApiResult<T> {
        let error = AppError::message(self.message);
        match self.details {
            Some(details) => ApiResult::err_with_details(error, details),
            None => ApiResult::err(error),
        }
    }
}

/// Structured `error.details` payload for rename transactions (same keys as the
/// desktop shell).
fn rename_error_details(error: &agentero_core::features::vault::rename::WikiRenameError) -> Value {
    serde_json::json!({
        "code": error.code,
        "rollback": error.rollback,
        "paths": error.paths,
    })
}

fn serialise<T: serde::Serialize>(result: ApiResult<T>) -> HandlerResult {
    serde_json::to_value(result).map_err(|error| format!("serialize_failed: {error}"))
}

/// Run a blocking body that answers a typed command (`ApiResult<T>` envelope).
async fn typed_blocking<T, F>(f: F) -> HandlerResult
where
    F: FnOnce() -> ApiResult<T> + Send + 'static,
    T: serde::Serialize + Send + 'static,
{
    match blocking(move || Ok(f())).await {
        Ok(result) => serialise(result),
        Err(error) => typed_err(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultPathArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BacklinksArgs {
    vault_path: String,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WikiResolveArgs {
    vault_path: String,
    source_path: String,
    link_text: String,
    #[serde(default)]
    syntax: Option<InternalLinkSyntax>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WikiEmbedArgs {
    vault_path: String,
    source_path: String,
    link_text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WikiSearchArgs {
    vault_path: String,
    query: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    kind: Option<WikiSearchCandidateKind>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WikiMoveArgs {
    vault_path: String,
    from_rel: String,
    to_rel: String,
    #[serde(default)]
    dirty_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WikiExternalRenamePreviewArgs {
    vault_path: String,
    from_rel: String,
    to_rel: String,
    #[serde(default)]
    dirty_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WikiApplyExternalRenameArgs {
    vault_path: String,
    candidate_id: String,
    #[serde(default)]
    dirty_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WikiRenameHeadingArgs {
    vault_path: String,
    path: String,
    heading_path: Vec<String>,
    heading_line: u32,
    expected_content: String,
    new_text: String,
    #[serde(default)]
    dirty_paths: Vec<String>,
}

/// Lock the shared index and make sure it is indexed for this vault.
fn lock_index_for(
    vault_path: &str,
) -> Result<std::sync::MutexGuard<'static, agentero_core::features::vault::rename::WikiIndex>, String>
{
    let mut guard = lock_wiki_index()?;
    guard.ensure_vault(vault_path)?;
    Ok(guard)
}

/// `graph_get_backlinks(vaultPath, path)` → `ApiResult<BacklinksResponse>`.
pub async fn graph_get_backlinks(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: BacklinksArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    typed_blocking(move || match vault_root(&args.vault_path) {
        Ok(vault) => {
            let vault_path = vault.to_string_lossy().to_string();
            match lock_index_for(&vault_path) {
                Ok(guard) => ApiResult::ok(guard.get_backlinks(&vault_path, &args.path)),
                Err(error) => BusinessError::new(error).into_api(),
            }
        }
        Err(error) => BusinessError::new(error).into_api(),
    })
    .await
}

/// `wiki_resolve(vaultPath, sourcePath, linkText, syntax)` →
/// `ApiResult<WikiResolveResponse>`.
pub async fn wiki_resolve(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: WikiResolveArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    typed_blocking(move || match vault_root(&args.vault_path) {
        Ok(vault) => {
            let vault_path = vault.to_string_lossy().to_string();
            match lock_index_for(&vault_path) {
                Ok(guard) => ApiResult::ok(guard.resolve_text(
                    &vault_path,
                    &args.source_path,
                    &args.link_text,
                    args.syntax.unwrap_or(InternalLinkSyntax::Wikilink),
                )),
                Err(error) => BusinessError::new(error).into_api(),
            }
        }
        Err(error) => BusinessError::new(error).into_api(),
    })
    .await
}

/// `wiki_embed_read(vaultPath, sourcePath, linkText)` → `ApiResult<WikiEmbedResponse>`.
pub async fn wiki_embed_read(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: WikiEmbedArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    typed_blocking(move || match vault_root(&args.vault_path) {
        Ok(vault) => {
            let vault_path = vault.to_string_lossy().to_string();
            match lock_index_for(&vault_path) {
                Ok(guard) => {
                    match guard.read_embed(&vault_path, &args.source_path, &args.link_text) {
                        Ok(response) => ApiResult::ok(response),
                        Err(error) => BusinessError::new(error).into_api(),
                    }
                }
                Err(error) => BusinessError::new(error).into_api(),
            }
        }
        Err(error) => BusinessError::new(error).into_api(),
    })
    .await
}

/// `wiki_search(vaultPath, query, path, kind)` → `ApiResult<WikiSearchCandidate[]>`.
pub async fn wiki_search(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: WikiSearchArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    typed_blocking(move || match vault_root(&args.vault_path) {
        Ok(vault) => {
            let vault_path = vault.to_string_lossy().to_string();
            match lock_index_for(&vault_path) {
                Ok(guard) => ApiResult::ok(guard.search_scoped(
                    &args.query,
                    args.path.as_deref(),
                    args.kind.as_ref(),
                )),
                Err(error) => BusinessError::new(error).into_api(),
            }
        }
        Err(error) => BusinessError::new(error).into_api(),
    })
    .await
}

/// `graph_rebuild(vaultPath)` → `ApiResult<RebuildResult>`.
pub async fn graph_rebuild(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: VaultPathArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    typed_blocking(move || match vault_root(&args.vault_path) {
        Ok(vault) => {
            let vault_path = vault.to_string_lossy().to_string();
            match lock_wiki_index() {
                Ok(mut guard) => match guard.rebuild(&vault_path) {
                    Ok(report) => ApiResult::ok(report),
                    Err(error) => BusinessError::new(error).into_api(),
                },
                Err(error) => BusinessError::new(error).into_api(),
            }
        }
        Err(error) => BusinessError::new(error).into_api(),
    })
    .await
}

/// `wiki_rename_heading(args)` → `ApiResult<WikiRenameHeadingResult>`.
pub async fn wiki_rename_heading(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: WikiRenameHeadingArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    typed_blocking(move || {
        let vault = match vault_root(&args.vault_path) {
            Ok(vault) => vault,
            Err(error) => return BusinessError::new(error).into_api(),
        };
        let mut guard = match lock_wiki_index() {
            Ok(guard) => guard,
            Err(error) => return BusinessError::new(error).into_api(),
        };
        match run_heading_rename_transaction(
            &vault,
            &mut guard,
            &args.path,
            &args.heading_path,
            args.heading_line,
            &args.expected_content,
            &args.new_text,
            &args.dirty_paths,
        ) {
            Ok(result) => ApiResult::ok(result),
            Err(error) => {
                BusinessError::with_details(error.to_string(), rename_error_details(&error))
                    .into_api()
            }
        }
    })
    .await
}

/// `wiki_move(args)` → `ApiResult<WikiRenameResult>`.
///
/// One filesystem move plus link rewrites; the dependent catalog commit runs
/// while the move is still recoverable (same orchestration as the desktop).
pub async fn wiki_move(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: WikiMoveArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    typed_blocking(move || {
        let vault = match vault_root(&args.vault_path) {
            Ok(vault) => vault,
            Err(error) => return BusinessError::new(error).into_api(),
        };
        let mut guard = match lock_wiki_index() {
            Ok(guard) => guard,
            Err(error) => return BusinessError::new(error).into_api(),
        };
        match run_local_rename_transaction(
            &vault,
            &mut guard,
            &args.from_rel,
            &args.to_rel,
            &args.dirty_paths,
            || {
                papers::move_under_path(&vault, &args.from_rel, &args.to_rel)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
        ) {
            Ok(result) => {
                usage::rename_path_best_effort(
                    &vault.to_string_lossy(),
                    &args.from_rel,
                    &args.to_rel,
                );
                // Renamed paper folders change tag-index paths.
                agentero_core::features::paper::catalog::tag_index::invalidate_tag_index(&vault);
                agentero_core::features::paper::catalog::tag_index::ensure_tag_index_best_effort(
                    &vault,
                );
                ApiResult::ok(result)
            }
            Err(error) => {
                BusinessError::with_details(error.to_string(), rename_error_details(&error))
                    .into_api()
            }
        }
    })
    .await
}

/// `wiki_external_rename_preview(args)` → `ApiResult<WikiExternalRenamePreview>`.
pub async fn wiki_external_rename_preview(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: WikiExternalRenamePreviewArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let repairs = repair_store();
    typed_blocking(move || {
        let vault = match vault_root(&args.vault_path) {
            Ok(vault) => vault,
            Err(error) => return BusinessError::new(error).into_api(),
        };
        let mut guard = match lock_wiki_index() {
            Ok(guard) => guard,
            Err(error) => return BusinessError::new(error).into_api(),
        };
        let vault_path = vault.to_string_lossy().to_string();
        if let Err(error) = guard.ensure_vault(&vault_path) {
            return BusinessError::new(error).into_api();
        }
        let transaction = match WikiRenameTransaction::plan_external_repair(
            &vault,
            &guard,
            &args.from_rel,
            &args.to_rel,
        ) {
            Ok(transaction) => transaction,
            Err(error) => {
                return BusinessError::with_details(error.to_string(), rename_error_details(&error))
                    .into_api()
            }
        };
        if let Err(error) = transaction.reject_dirty_paths(&args.dirty_paths) {
            return BusinessError::with_details(error.to_string(), rename_error_details(&error))
                .into_api();
        }
        let affected_sources = transaction.updated_sources();
        let skipped = transaction.skipped().to_vec();
        let from = transaction.from().to_string();
        let to = transaction.moved_path().to_string();
        drop(guard);
        match repairs.insert(transaction) {
            Ok(candidate_id) => ApiResult::ok(WikiExternalRenamePreview {
                candidate_id,
                from,
                to,
                affected_sources,
                skipped,
            }),
            Err(error) => {
                BusinessError::with_details(error.to_string(), rename_error_details(&error))
                    .into_api()
            }
        }
    })
    .await
}

/// `wiki_apply_external_rename_repair(args)` → `ApiResult<WikiRenameResult>`.
pub async fn wiki_apply_external_rename_repair(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: WikiApplyExternalRenameArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let repairs = repair_store();
    typed_blocking(move || {
        let vault = match vault_root(&args.vault_path) {
            Ok(vault) => vault,
            Err(error) => return BusinessError::new(error).into_api(),
        };
        let transaction = match repairs.get(&args.candidate_id) {
            Ok(transaction) => transaction,
            Err(error) => {
                return BusinessError::with_details(error.to_string(), rename_error_details(&error))
                    .into_api()
            }
        };
        let mut guard = match lock_wiki_index() {
            Ok(guard) => guard,
            Err(error) => return BusinessError::new(error).into_api(),
        };
        match run_prepared_external_rename_repair(
            &vault,
            &mut guard,
            &transaction,
            &args.dirty_paths,
        ) {
            Ok(result) => {
                repairs.remove(&args.candidate_id);
                ApiResult::ok(result)
            }
            Err(error) => {
                if error.code != WikiRenameErrorCode::UnsavedEdits {
                    repairs.remove(&args.candidate_id);
                }
                BusinessError::with_details(error.to_string(), rename_error_details(&error))
                    .into_api()
            }
        }
    })
    .await
}
