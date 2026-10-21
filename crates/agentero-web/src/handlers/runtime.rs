//! Runtime handler registry (jobs / tasks / watcher / sync).
//!
//! Owned by the runtime layer; `rpc.rs` asks it after the domain registry.

use serde_json::Value;

use crate::error::HandlerResult;
use crate::rpc::RpcCtx;

pub async fn dispatch(_ctx: &RpcCtx, _command: &str, _args: &Value) -> Option<HandlerResult> {
    // jobs / sync / watcher commands register here.
    None
}
