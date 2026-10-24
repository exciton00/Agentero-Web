//! Session-scoped stdio RAG MCP worker.
//!
//! The `agentero-web` process re-executes itself as
//! `agentero-web --agentero-rag-mcp <session-config.json>` when an ACP agent
//! spawns the `agentero-rag` MCP server for one turn (see
//! `agentero_host::context_turn::attach_rag_mcp`). It speaks MCP over stdio and
//! exposes only the three read-only WeKnora tools.
//!
//! The config file is written by the host with mode 0600, carries the API key,
//! is never logged, and is deleted when the ACP turn ends.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};

use crate::mcp::rag_tools::{
    self, RagPaperContextArgs, RagPaperContextOut, RagRuntime, RagSearchArgs, RagSearchOut,
    RagSyncStatusArgs, RagSyncStatusOut,
};

#[derive(Clone)]
struct AgenteroRagMcp {
    runtime: Arc<RagRuntime>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl AgenteroRagMcp {
    fn new(runtime: RagRuntime) -> Self {
        Self {
            runtime: Arc::new(runtime),
            tool_router: Self::tool_router(),
        }
    }
}

fn tool_err(error: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        serde_json::json!({
            "code": "rag_error",
            "message": error.to_string(),
        })
        .to_string(),
    )])
}

#[tool_router]
impl AgenteroRagMcp {
    #[tool(
        description = "Read-only semantic + keyword search across the RAG knowledge base synced from this Vault (paper bodies, NOTES, figure captions, annotations). Use it for cross-library questions like 'do my notes discuss X?'. Hits carry paper.path (usable as `ref`) and a BibTeX key with latex '\\\\cite{key}'. For reading one specific paper use paper_get / page_read instead.",
        annotations(read_only_hint = true)
    )]
    async fn agentero_rag_search(
        &self,
        Parameters(args): Parameters<RagSearchArgs>,
    ) -> Result<Json<RagSearchOut>, CallToolResult> {
        self.runtime.search(args).await.map(Json).map_err(tool_err)
    }

    #[tool(
        description = "Read-only search restricted to ONE synced paper (its body, user notes, annotations and figure/table captions). Returns the section each hit came from. Use paper_get/page_read to read the paper itself.",
        annotations(read_only_hint = true)
    )]
    async fn agentero_rag_paper_context(
        &self,
        Parameters(args): Parameters<RagPaperContextArgs>,
    ) -> Result<Json<RagPaperContextOut>, CallToolResult> {
        self.runtime
            .paper_context(args)
            .await
            .map(Json)
            .map_err(tool_err)
    }

    #[tool(
        description = "Report whether the RAG knowledge base is usable for this Vault: enabled/configured, synced x/y, last sync time, and (with `paper`) that paper's sync status/lastError. Read-only; no network call.",
        annotations(read_only_hint = true)
    )]
    async fn agentero_rag_sync_status(
        &self,
        Parameters(args): Parameters<RagSyncStatusArgs>,
    ) -> Result<Json<RagSyncStatusOut>, CallToolResult> {
        self.runtime.sync_status(args).map(Json).map_err(tool_err)
    }
}

#[tool_handler]
impl ServerHandler for AgenteroRagMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "agentero-rag is a read-only, session-scoped RAG server for this Vault. \
                 Use rag_search for cross-library semantic questions and agentero_rag_paper_context \
                 for one paper's notes/figures/annotations; both return paper.path and the BibTeX \
                 key/\\cite{} to quote. Use paper_get/page_read (Agentero vault tools) to read a \
                 paper directly. It never writes to the provider.",
            )
            .with_server_info(Implementation::new(
                "agentero-rag",
                env!("CARGO_PKG_VERSION"),
            ))
    }
}

/// Exit into the stdio worker when this process was spawned as `agentero-rag`.
/// `None` means a normal Web host launch.
pub fn try_run_rag_mcp() -> Option<i32> {
    let mut args = std::env::args();
    let _argv0 = args.next();
    if args.next().as_deref() != Some(agentero_host::models::RAG_MCP_WORKER_ARG) {
        return None;
    }
    let Some(config) = args.next() else {
        eprintln!("agentero-rag: missing session config path");
        return Some(2);
    };
    if args.next().is_some() {
        eprintln!("agentero-rag: unexpected arguments");
        return Some(2);
    }
    match serve(PathBuf::from(config)) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("agentero-rag: {error}");
            Some(1)
        }
    }
}

fn serve(config_path: PathBuf) -> Result<(), String> {
    let runtime = rag_tools::load_worker_runtime(&config_path)
        .map_err(|error| error.to_string())?;
    if !runtime.enabled() {
        return Err("WeKnora RAG is disabled or not configured".into());
    }
    let mut runtime_builder = tokio::runtime::Builder::new_current_thread();
    runtime_builder.enable_all();
    let async_runtime = runtime_builder
        .build()
        .map_err(|error| format!("start rag mcp runtime: {error}"))?;
    async_runtime.block_on(async move {
        let server = AgenteroRagMcp::new(runtime)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|error| format!("rag mcp handshake: {error}"))?;
        server
            .waiting()
            .await
            .map_err(|error| format!("rag mcp stopped: {error}"))?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::AgenteroRagMcp;
    use crate::mcp::rag_tools::RagRuntime;
    use agentero_core::features::rag::settings::RagSettings;
    use rmcp::model::CallToolRequestParams;
    use rmcp::ServiceExt;

    fn enabled_runtime(vault: &std::path::Path) -> RagRuntime {
        RagRuntime::new(
            RagSettings {
                enabled: true,
                base_url: "https://kb.example.com".into(),
                knowledge_base_id: "kb-1".into(),
                ..Default::default()
            },
            vault,
        )
    }

    #[tokio::test]
    async fn worker_exposes_three_read_only_tools_and_answers_local_status() {
        let vault = tempfile::tempdir().unwrap();
        let (server_io, client_io) = tokio::io::duplex(65536);
        let runtime = enabled_runtime(vault.path());
        let server = tokio::spawn(async move {
            AgenteroRagMcp::new(runtime)
                .serve(server_io)
                .await
                .unwrap()
        });
        let mut client = ().serve(client_io).await.unwrap();
        let mut server = server.await.unwrap();

        let peer = client.peer_info().unwrap();
        assert_eq!(
            peer.server_info.as_ref().unwrap().name,
            "agentero-rag"
        );
        let tools = client.list_all_tools().await.unwrap();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(tools.len(), 3, "{names:?}");
        for name in ["agentero_rag_search", "agentero_rag_paper_context", "agentero_rag_sync_status"] {
            assert!(names.contains(&name), "missing {name}");
        }
        let status = client
            .call_tool(CallToolRequestParams::new("agentero_rag_sync_status"))
            .await
            .unwrap();
        assert_ne!(status.is_error, Some(true), "{status:?}");
        let out = status.structured_content.unwrap();
        assert_eq!(out["enabled"], true);
        assert_eq!(out["total"], 0);

        client.close().await.unwrap();
        server.close().await.unwrap();
    }
}
