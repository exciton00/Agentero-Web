//! Domain handler registry.
//!
//! Feature work appends `command => handler` arms here so the dispatch table in
//! `rpc.rs` (owned by the platform layer) stays untouched. Return `None` for
//! commands this module does not own; `rpc.rs` then falls back to the platform
//! commands and finally to the stub table.

use serde_json::Value;

use crate::error::HandlerResult;
use crate::rpc::RpcCtx;

pub async fn dispatch(_ctx: &RpcCtx, _command: &str, _args: &Value) -> Option<HandlerResult> {
    // M1: files / markdown / paper mutations register here.
    None
}
