//! Attach one turn's context plan to the ACP prompt and, locally, to stdio MCP.

use crate::core::error::AppError;
use crate::models::{RagMcpSpec, RAG_MCP_WORKER_ARG};
use agent_client_protocol::schema::v1::{McpServer, McpServerStdio};
use agentero_core::features::context::{
    facts_from_anchors, grant_path, plan_from_facts, render_prompt_block, resolve_in_vault,
    resume_allowed, session_fingerprint, usage_path, write_grant, ContextExpansion, ContextGrant,
    ContextPlan, ContextRequest, GRANT_SCHEMA_VERSION, MCP_WORKER_ARG,
};
use std::path::{Path, PathBuf};

/// What `run_once` needs from the context layer. Absent when the turn has no anchors.
pub(crate) struct PreparedContext {
    pub block: String,
    pub mcp_servers: Vec<McpServer>,
    pub fingerprint: Option<String>,
    pub usage_path: Option<PathBuf>,
    pub vault: Option<PathBuf>,
    /// A warm ACP session was created without this MCP server. Do not reuse it.
    pub skip_warm_pool: bool,
    pub resume_session_id: Option<String>,
}

pub(crate) fn prepare_context(
    runtime_session_id: &str,
    vault_path: Option<&str>,
    remote: bool,
    request: Option<&ContextRequest>,
    resume_session_id: Option<String>,
) -> PreparedContext {
    let Some(request) = request.filter(|request| !request.anchors.is_empty()) else {
        return PreparedContext {
            block: String::new(),
            mcp_servers: Vec::new(),
            fingerprint: None,
            usage_path: None,
            vault: None,
            skip_warm_pool: false,
            resume_session_id,
        };
    };
    let vault = vault_path.map(PathBuf::from).filter(|path| path.is_dir());
    let mut expansion = if remote || vault.is_none() {
        ContextExpansion::PromptOnly
    } else {
        ContextExpansion::Mcp
    };
    let mut plan = match vault.as_deref() {
        Some(vault) => resolve_in_vault(vault, request, expansion).unwrap_or_else(|error| {
            log::warn!(target: "agentero::agent", "context resolve failed: {error}");
            plan_from_facts(
                request,
                &facts_from_anchors(&request.anchors),
                ContextExpansion::PromptOnly,
            )
        }),
        None => plan_from_facts(
            request,
            &facts_from_anchors(&request.anchors),
            ContextExpansion::PromptOnly,
        ),
    };
    if plan.expansion == ContextExpansion::PromptOnly {
        expansion = ContextExpansion::PromptOnly;
    }
    let mut mcp_servers = Vec::new();
    let mut usage = None;
    if expansion == ContextExpansion::Mcp {
        if let Some(vault) = vault.as_deref() {
            match attach_mcp(vault, runtime_session_id, &plan) {
                Ok((servers, usage_path)) => {
                    mcp_servers = servers;
                    usage = Some(usage_path);
                }
                Err(error) => {
                    log::warn!(
                        target: "agentero::agent",
                        "context mcp unavailable, prompt-only fallback: {error}"
                    );
                    match resolve_in_vault(vault, request, ContextExpansion::PromptOnly) {
                        Ok(resolved) => plan = resolved,
                        Err(_) => plan.expansion = ContextExpansion::PromptOnly,
                    }
                }
            }
        }
    }
    let fingerprint = plan.fingerprint.clone();
    let resume_session_id = match vault.as_deref() {
        Some(vault) => gate_resume(vault, resume_session_id, &fingerprint),
        None => resume_session_id,
    };
    let block = render_prompt_block(&plan);
    PreparedContext {
        block,
        mcp_servers,
        fingerprint: Some(fingerprint),
        usage_path: usage,
        vault,
        skip_warm_pool: plan.expansion == ContextExpansion::Mcp,
        resume_session_id,
    }
}

fn gate_resume(
    vault: &Path,
    resume_session_id: Option<String>,
    fingerprint: &str,
) -> Option<String> {
    let resume = resume_session_id?;
    let stored = session_fingerprint(vault, &resume);
    if resume_allowed(stored.as_deref(), fingerprint) {
        Some(resume)
    } else {
        log::info!(
            target: "agentero::agent",
            "context fingerprint changed; starting a new ACP session"
        );
        None
    }
}

fn attach_mcp(
    vault: &Path,
    runtime_session_id: &str,
    plan: &ContextPlan,
) -> Result<(Vec<McpServer>, PathBuf), AppError> {
    let grant_file = grant_path(vault, runtime_session_id);
    let grant = ContextGrant {
        schema_version: GRANT_SCHEMA_VERSION,
        session_id: runtime_session_id.to_string(),
        vault_path: vault.display().to_string(),
        fingerprint: plan.fingerprint.clone(),
        allowed: plan.allowed.clone(),
        budget: plan.budget.clone(),
        mode: plan.mode,
    };
    write_grant(&grant_file, &grant)?;
    let exe = std::env::current_exe()
        .map_err(|error| AppError::message(format!("locate agentero for context mcp: {error}")))?;
    let stdio = McpServerStdio::new("agentero-context", exe).args(vec![
        MCP_WORKER_ARG.to_string(),
        grant_file.display().to_string(),
    ]);
    Ok((
        vec![McpServer::Stdio(stdio)],
        usage_path(vault, runtime_session_id),
    ))
}

/// Short per-turn hint appended only when the RAG MCP server is injected.
pub(crate) const RAG_PROMPT_HINT: &str = "RAG tools (agentero_rag_search / agentero_rag_paper_context / agentero_rag_sync_status) are available read-only over the RAG knowledge base synced from this Vault. Use them for cross-library questions such as \"do my notes or figure captions discuss X?\"; every hit carries paper.path (usable as `ref`), a provider id and a BibTeX key/\\cite{}. Do not use them to read one specific paper (use paper_get/page_read).";

/// 0600 worker config written per ACP turn and removed when the turn ends.
pub(crate) struct RagConfigGuard {
    path: PathBuf,
}

impl Drop for RagConfigGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RagWorkerConfig<'a> {
    provider: &'a str,
    base_url: &'a str,
    api_key: &'a str,
    knowledge_base_id: &'a str,
    vault_path: &'a str,
    state_path: &'a str,
    rerank: bool,
    timeout_ms: u64,
    top_k: u32,
}

/// Build the `agentero-rag` stdio MCP descriptor for this turn.
///
/// The API key travels only through the 0600 temp config file; it is never
/// logged and never passed on the command line.
pub(crate) fn attach_rag_mcp(
    spec: &RagMcpSpec,
    runtime_session_id: &str,
) -> Result<(McpServer, RagConfigGuard), AppError> {
    let dir = std::env::temp_dir().join("agentero-rag-mcp");
    std::fs::create_dir_all(&dir).map_err(|error| {
        AppError::message(format!("create rag mcp config dir {}: {error}", dir.display()))
    })?;
    let path = dir.join(format!("{runtime_session_id}.json"));
    let config = RagWorkerConfig {
        provider: spec.provider.trim(),
        base_url: spec.base_url.trim(),
        api_key: spec.api_key.trim(),
        knowledge_base_id: spec.knowledge_base_id.trim(),
        vault_path: spec.vault_path.trim(),
        state_path: spec.state_path.trim(),
        rerank: spec.rerank,
        timeout_ms: spec.timeout_ms,
        top_k: spec.top_k,
    };
    let bytes = serde_json::to_vec(&config)
        .map_err(|error| AppError::message(format!("serialize rag mcp config: {error}")))?;
    write_private_file(&path, &bytes)?;
    let exe = std::env::current_exe()
        .map_err(|error| AppError::message(format!("locate agentero for rag mcp: {error}")))?;
    let stdio = McpServerStdio::new("agentero-rag", exe).args(vec![
        RAG_MCP_WORKER_ARG.to_string(),
        path.display().to_string(),
    ]);
    Ok((
        McpServer::Stdio(stdio),
        RagConfigGuard { path },
    ))
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            AppError::message(format!("create rag mcp config {}: {error}", path.display()))
        })?;
    file.write_all(bytes).map_err(|error| {
        AppError::message(format!("write rag mcp config {}: {error}", path.display()))
    })
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| {
            AppError::message(format!("create rag mcp config {}: {error}", path.display()))
        })?;
    file.write_all(bytes).map_err(|error| {
        AppError::message(format!("write rag mcp config {}: {error}", path.display()))
    })
}

/// Insert the evidence block before the human request so chat history still
/// shows only what the user typed.
pub(crate) fn insert_context_block(prompt: &str, block: &str) -> String {
    let block = block.trim();
    if block.is_empty() {
        return prompt.to_string();
    }
    const MARKER: &str = crate::features::agent::prompt::envelope::USER_REQUEST_MARKER;
    match prompt.rfind(MARKER) {
        Some(index) => {
            let mut out = String::with_capacity(prompt.len() + block.len() + 2);
            out.push_str(&prompt[..index]);
            out.push_str(block);
            out.push_str("\n\n");
            out.push_str(&prompt[index..]);
            out
        }
        None => format!("{block}\n\n{prompt}"),
    }
}

/// Prior turns for a session the fingerprint refused to resume. They sit inside
/// the user request, after the evidence block, so the new session still has the
/// conversation.
pub(crate) fn insert_resume_transcript(prompt: &str, transcript: &str) -> String {
    let transcript = transcript.trim();
    if transcript.is_empty() {
        return prompt.to_string();
    }
    const MARKER: &str = crate::features::agent::prompt::envelope::USER_REQUEST_MARKER;
    match prompt.rfind(MARKER) {
        Some(index) => {
            let at = index + MARKER.len();
            let mut out = String::with_capacity(prompt.len() + transcript.len() + 2);
            out.push_str(&prompt[..at]);
            out.push_str(transcript);
            out.push_str("\n\n");
            out.push_str(&prompt[at..]);
            out
        }
        None => format!("{transcript}\n\n{prompt}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{insert_context_block, insert_resume_transcript};
    use crate::models::RagMcpSpec;

    fn spec() -> RagMcpSpec {
        RagMcpSpec {
            provider: "weknora".into(),
            base_url: "https://kb.example.com".into(),
            api_key: "test-secret".into(),
            knowledge_base_id: "kb-1".into(),
            vault_path: "/tmp/vault".into(),
            state_path: "/tmp/state.json".into(),
            rerank: false,
            timeout_ms: 15_000,
            top_k: 8,
        }
    }

    #[test]
    fn rag_spec_requires_base_kb_and_vault() {
        assert!(spec().is_complete());
        let mut incomplete = spec();
        incomplete.knowledge_base_id.clear();
        assert!(!incomplete.is_complete());
        let mut incomplete = spec();
        incomplete.base_url = "  ".into();
        assert!(!incomplete.is_complete());
    }

    #[test]
    fn attach_rag_mcp_writes_private_config_and_cleans_it_up() {
        let attached = super::attach_rag_mcp(&spec(), "rag-test-session").unwrap();
        let (server, guard) = attached;
        let agent_client_protocol::schema::v1::McpServer::Stdio(stdio) = server else {
            panic!("expected stdio server");
        };
        assert_eq!(stdio.name, "agentero-rag");
        let args = stdio.args.clone();
        assert_eq!(args.first().map(String::as_str), Some("--agentero-rag-mcp"));
        assert!(
            args.get(1)
                .is_some_and(|arg| arg.ends_with("rag-test-session.json")),
            "{args:?}"
        );
        let path = guard.path.clone();
        assert!(path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "worker config must be private");
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("kb-1"));
        assert!(raw.contains("/tmp/vault"));
        assert!(raw.contains("test-secret"), "key only inside the 0600 file");
        drop(guard);
        assert!(!path.exists(), "config removed when the turn ends");
    }

    #[test]
    fn evidence_stays_above_the_user_request() {
        let prompt = "You are an assistant.\n\nUser request:\nwhat does this mean?";
        let out = insert_context_block(prompt, "Paper ref: attention\nTitle: Attention");
        let evidence = out.find("Paper ref: attention").unwrap();
        let request = out.find("User request:\n").unwrap();
        assert!(evidence < request);
        assert!(out.ends_with("what does this mean?"));
    }

    #[test]
    fn rejected_resume_keeps_earlier_turns_inside_the_request() {
        let prompt = insert_context_block(
            "You are an assistant.\n\nUser request:\nwhat next?",
            "Paper ref: attention",
        );
        let out = insert_resume_transcript(
            &prompt,
            "Earlier turns in this conversation:\n\nUser: hello",
        );
        let request = out.find("User request:\n").unwrap();
        let earlier = out.find("Earlier turns").unwrap();
        let question = out.find("what next?").unwrap();
        assert!(out.find("Paper ref: attention").unwrap() < request);
        assert!(request < earlier && earlier < question);
    }
}
