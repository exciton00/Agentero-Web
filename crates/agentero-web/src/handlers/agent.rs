//! Agent (BYOA/ACP) RPC handlers + host/vault/network doctor commands.
//!
//! Shells over [`agentero_host::service::AgentHost`] (the tauri-free port of
//! `src-tauri/src/features/agent`) plus the shared `agentero_core` vault doctor
//! services. Wire shapes follow `src/lib/core/bindings.ts` exactly:
//!
//! - plain commands answer `{ok, data | error}`;
//! - `typedError` commands answer the same inner envelope on HTTP 200 and only
//!   use a transport-level failure when the blocking task dies.
//!
//! Events (`agent:*`, `agent-lifecycle:*`) already flow through
//! `agent_runtime::EventBusSink`; `agent_run_once` / `agent_warm` pass the
//! request's tab label so they reach only the starting browser tab.
//!
//! Desktop-only surface (`*_terminal` commands, `remote_agent_*`) answers a
//! stable `unsupported` business error.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use agentero_core::error::{ApiResult, AppError};
use agentero_core::features::vault::doctor as vault_doctor;
use agentero_core::http::{self, BROWSER_USER_AGENT, DEFAULT_REDIRECT_LIMIT};
use agentero_host::models::{
    AskUserResponseRequest, ElicitationResponseRequest, PermissionResponseRequest, ProbeResult,
    RunOnceRequest, UpsertAgentRequest, WarmRequest,
};
use agentero_host::service::AgentHost;
use futures_util::future::join_all;

use crate::error::{fail, ok, parse_args, typed_err, typed_ok, vault_root, HandlerResult};
use crate::handlers::wiki_state::lock_wiki_index;
use crate::rpc::RpcCtx;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Convert a domain result into the command envelope (`ApiResult`).
fn api<T: Serialize>(result: Result<T, AppError>) -> ApiResult<T> {
    match result {
        Ok(value) => ApiResult::ok(value),
        Err(error) => ApiResult::err(error),
    }
}

/// Business failure carrying a structured `error.details` payload (repair
/// transactions), matching the desktop `ApiResult::err_with_details` shape.
fn fail_with_details(message: impl Into<String>, details: Value) -> HandlerResult {
    let result = ApiResult::<()>::err_with_details(AppError::message(message), details);
    serde_json::to_value(result).map_err(|error| format!("serialize_failed: {error}"))
}

/// Serialize an already-built envelope.
fn envelope<T: Serialize>(result: ApiResult<T>) -> HandlerResult {
    serde_json::to_value(result).map_err(|error| format!("serialize_failed: {error}"))
}

/// `unsupported` business error for desktop-only commands.
fn unsupported(message: impl Into<String>) -> HandlerResult {
    fail(AppError::domain("unsupported", message))
}

/// Run one blocking [`AgentHost`] call on the blocking pool and answer the
/// envelope it returns.
async fn blocking_agent<T, F>(ctx: &RpcCtx, work: F) -> HandlerResult
where
    F: FnOnce(Arc<AgentHost>) -> ApiResult<T> + Send + 'static,
    T: Serialize + Send + 'static,
{
    let host = ctx.state.agents.clone();
    match tokio::task::spawn_blocking(move || work(host)).await {
        Ok(result) => envelope(result),
        Err(error) => typed_err(format!("agent blocking task failed: {error}")),
    }
}

/// Blocking closure returning `(message, details?)` on failure; used by the
/// vault-doctor repair commands whose desktop shells attach details.
type DetailedRepair<T> = Result<T, (String, Option<Value>)>;

async fn blocking_repair<T, F>(work: F) -> HandlerResult
where
    F: FnOnce() -> DetailedRepair<T> + Send + 'static,
    T: Serialize + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => typed_ok(value),
        Ok(Err((message, Some(details)))) => fail_with_details(message, details),
        Ok(Err((message, None))) => fail(AppError::message(message)),
        Err(error) => typed_err(format!("doctor blocking task failed: {error}")),
    }
}

fn detailed_error<T>(error: &impl Serialize, message: String) -> DetailedRepair<T> {
    Err((
        message,
        Some(serde_json::to_value(error).unwrap_or(Value::Null)),
    ))
}

// ---------------------------------------------------------------------------
// argument shapes (camelCase, exactly what bindings.ts sends)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentIdArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TemplateIdArgs {
    template_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentSkillsArgs {
    #[serde(default)]
    vault_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveAgentArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetDefaultArgs {
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnsureCatalogArgs {
    template_id: String,
    set_default: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetUserAgentArgs {
    user_agent: String,
    user_agent_provider_ids: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpsertAgentArgs {
    request: UpsertAgentRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskIdArgs {
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleCancelArgs {
    task_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolLifecycleArgs {
    template_id: String,
    action: String,
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartialUninstallArgs {
    template_id: String,
    scope: String,
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermissionArgs {
    request: PermissionResponseRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ElicitationArgs {
    request: ElicitationResponseRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskUserArgs {
    request: AskUserResponseRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CitationArgs {
    #[serde(default)]
    vault_path: Option<String>,
    source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunOnceArgs {
    request: RunOnceRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListSessionsArgs {
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    vault_path: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadSessionArgs {
    #[serde(default)]
    agent_id: Option<String>,
    session_id: String,
    #[serde(default)]
    vault_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WarmArgs {
    request: WarmRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelRunArgs {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAgentScanArgs {
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAgentProbeArgs {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    template_id: Option<String>,
}

/// `agent_set_user_agent` response (`AgentUserAgentResponse` in commands/mod.rs).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentUserAgentResponse {
    user_agent: String,
    user_agent_provider_ids: String,
}

// ---------------------------------------------------------------------------
// registry / catalog
// ---------------------------------------------------------------------------

/// `agent_list_agents` → `ApiResult<AgentListResponse>`.
async fn list_agents(ctx: &RpcCtx, _args: &Value) -> HandlerResult {
    blocking_agent(ctx, move |host| api(host.list_agents())).await
}

/// `host_agent_redetect` — clear the auto-registration marker and retry host
/// Dsh discovery. Used by the Settings button when an old data volume carries a
/// stale `.host-agent-auto.flag`.
async fn host_agent_redetect(ctx: &RpcCtx) -> HandlerResult {
    let marker = ctx
        .state
        .settings
        .path()
        .with_file_name(".host-agent-auto.flag");
    match std::fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::warn!("host_agent_redetect: remove {}: {error}", marker.display()),
    }
    let settings_path = ctx.state.settings.path().to_path_buf();
    let agents = ctx.state.agents.clone();
    if !crate::host_agent::ensure_host_dsh(&agents, &settings_path) {
        log::warn!("host_agent_redetect: no host Dsh detected");
    }
    blocking_agent(ctx, move |host| api(host.list_agents())).await
}

/// `agent_list_skills({vaultPath})` → `ApiResult<AgentSkill[]>`.
async fn list_skills(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: AgentSkillsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_agent(ctx, move |host| {
        ApiResult::ok(host.list_skills(args.vault_path.as_deref()))
    })
    .await
}

/// `agent_scan_catalog` → `ApiResult<CatalogScanResponse>`.
async fn scan_catalog(ctx: &RpcCtx, _args: &Value) -> HandlerResult {
    blocking_agent(ctx, move |host| api(host.scan_catalog())).await
}

/// `agent_check_catalog_updates` → `ApiResult<CatalogScanResponse>`.
async fn check_catalog_updates(ctx: &RpcCtx, _args: &Value) -> HandlerResult {
    envelope(api(ctx.state.agents.check_catalog_updates().await))
}

/// `agent_upsert_agent({request})` → `ApiResult<AgentOnly>`.
async fn upsert_agent(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: UpsertAgentArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_agent(ctx, move |host| api(host.upsert_agent(args.request))).await
}

/// `agent_ensure_catalog({templateId, setDefault})` → `ApiResult<AgentOnly>`.
async fn ensure_catalog(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: EnsureCatalogArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_agent(ctx, move |host| {
        api(host.ensure_catalog(&args.template_id, args.set_default))
    })
    .await
}

/// `agent_remove_agent({id})` → `ApiResult<null>`.
async fn remove_agent(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: RemoveAgentArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_agent(ctx, move |host| {
        api(host.remove_agent(&args.id).map(|()| Value::Null))
    })
    .await
}

/// `agent_set_default({id})` → `ApiResult<AgentListResponse>`.
async fn set_default(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SetDefaultArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_agent(ctx, move |host| api(host.set_default(args.id))).await
}

/// `agent_set_user_agent({userAgent, userAgentProviderIds})` →
/// `ApiResult<AgentUserAgentResponse>`.
async fn set_user_agent(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: SetUserAgentArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_agent(ctx, move |host| {
        match host.set_user_agent(args.user_agent, args.user_agent_provider_ids) {
            Ok(_) => match host.registry().snapshot() {
                Ok(state) => ApiResult::ok(AgentUserAgentResponse {
                    user_agent: state.user_agent,
                    user_agent_provider_ids: state.user_agent_provider_ids,
                }),
                Err(error) => ApiResult::err(error),
            },
            Err(error) => ApiResult::err(error),
        }
    })
    .await
}

/// `agent_probe({id})` → `ApiResult<ProbeResult>`.
///
/// Mirrors the desktop shell: an agent whose command is not on PATH answers a
/// synthetic `available: false` result without spawning anything.
async fn probe(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: AgentIdArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let host = ctx.state.agents.clone();
    let descriptor = match host.registry().get(&args.id) {
        Ok(descriptor) => descriptor,
        Err(error) => return fail(error),
    };
    if !descriptor.available {
        let result = ProbeResult {
            agent_id: args.id.clone(),
            available: false,
            agent_name: None,
            protocol_version: None,
            error: descriptor
                .last_error
                .clone()
                .or_else(|| Some(format!("command `{}` not found on PATH", descriptor.command))),
            session_capabilities: None,
        };
        let _ = host.registry().apply_probe_result(&args.id, &result);
        host.emit_registry_changed();
        return envelope(ApiResult::ok(result));
    }
    envelope(api(host.probe(&args.id).await))
}

/// `agent_probe_catalog({templateId})` → `ApiResult<ProbeResult>`.
async fn probe_catalog(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: TemplateIdArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(api(ctx.state.agents.probe_catalog(&args.template_id).await))
}

/// `doctor_check_host` → `ApiResult<HostDoctorReport>`.
async fn doctor_check_host(ctx: &RpcCtx, _args: &Value) -> HandlerResult {
    envelope(api(ctx.state.agents.doctor_check_host().await))
}

/// `doctor_install_node({taskId})` → `ApiResult<NodeInstallResult>`.
async fn doctor_install_node(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: TaskIdArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(
        api(
            ctx.state
                .agents
                .doctor_install_node(args.task_id.as_deref())
                .await,
        ),
    )
}

/// `doctor_check_agents` → `ApiResult<AgentAcpDiagnostic[]>`.
async fn doctor_check_agents(ctx: &RpcCtx, _args: &Value) -> HandlerResult {
    envelope(api(ctx.state.agents.doctor_check_agents().await))
}

// ---------------------------------------------------------------------------
// sessions
// ---------------------------------------------------------------------------

/// `agent_run_once({request})` → `ApiResult<RunOnceAccepted>`.
///
/// The browser tab label targets the turn's `agent:*` events back to it.
async fn run_once(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: RunOnceArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(
        api(
            ctx.state
                .agents
                .accept_run_once(ctx.window.clone(), args.request)
                .await,
        ),
    )
}

/// `agent_cancel_run({sessionId})` → `ApiResult<boolean>`.
async fn cancel_run(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: CancelRunArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(api(ctx.state.agents.cancel_run(&args.session_id)))
}

/// `agent_warm({request})` → `ApiResult<WarmResult>`.
async fn warm(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: WarmArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let result = ctx
        .state
        .agents
        .warm(ctx.window.clone(), args.request)
        .await;
    envelope(ApiResult::ok(result))
}

/// `agent_list_sessions({agentId, vaultPath, cursor})` →
/// `ApiResult<AcpListSessionsResult>`.
async fn list_sessions(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ListSessionsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(
        api(
            ctx.state
                .agents
                .list_sessions(args.agent_id, args.vault_path, args.cursor)
                .await,
        ),
    )
}

/// `agent_load_session({agentId, sessionId, vaultPath})` →
/// `ApiResult<AcpLoadSessionResult>`.
async fn load_session(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: LoadSessionArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(
        api(
            ctx.state
                .agents
                .load_session(args.agent_id, args.session_id, args.vault_path)
                .await,
        ),
    )
}

// ---------------------------------------------------------------------------
// user gates
// ---------------------------------------------------------------------------

/// `agent_respond_permission({request})` → `ApiResult<PermissionResponded>`.
async fn respond_permission(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PermissionArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(ApiResult::ok(
        ctx.state.agents.respond_permission(args.request),
    ))
}

/// `agent_respond_elicitation({request})` → `ApiResult<PermissionResponded>`.
async fn respond_elicitation(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ElicitationArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(ApiResult::ok(
        ctx.state.agents.respond_elicitation(args.request),
    ))
}

/// `agent_respond_ask_user({request})` → `ApiResult<PermissionResponded>`.
async fn respond_ask_user(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: AskUserArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(ApiResult::ok(
        ctx.state.agents.respond_ask_user(args.request),
    ))
}

// ---------------------------------------------------------------------------
// citation / lifecycle
// ---------------------------------------------------------------------------

/// `agent_resolve_citation({vaultPath, source})` → `ApiResult<CitationTarget>`.
async fn resolve_citation(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: CitationArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_agent(ctx, move |host| {
        api(host.resolve_citation(args.vault_path.as_deref(), &args.source))
    })
    .await
}

/// `agent_run_tool_lifecycle({templateId, action, taskId})` →
/// `ApiResult<null>`.
async fn run_tool_lifecycle(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: ToolLifecycleArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_agent(ctx, move |host| {
        api(
            host.run_tool_lifecycle(&args.template_id, &args.action, args.task_id.as_deref())
                .map(|()| Value::Null),
        )
    })
    .await
}

/// `agent_run_partial_uninstall({templateId, scope, taskId})` →
/// `ApiResult<null>`.
async fn run_partial_uninstall(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: PartialUninstallArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_agent(ctx, move |host| {
        api(
            host.run_partial_uninstall(&args.template_id, &args.scope, args.task_id.as_deref())
                .map(|()| Value::Null),
        )
    })
    .await
}

/// `agent_lifecycle_cancel({taskId})` → `ApiResult<boolean>`.
async fn lifecycle_cancel(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: LifecycleCancelArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(ApiResult::ok(
        ctx.state.agents.cancel_tool_lifecycle(&args.task_id),
    ))
}

/// `agent_tool_uninstall_info({templateId})` → `ApiResult<UninstallInfo | null>`.
async fn tool_uninstall_info(ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: TemplateIdArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    envelope(ApiResult::ok(
        ctx.state.agents.tool_uninstall_info(&args.template_id),
    ))
}

// ---------------------------------------------------------------------------
// network doctor (settings page diagnostics)
// ---------------------------------------------------------------------------

const NETWORK_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const NETWORK_DETAIL_MAX_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum EndpointId {
    Baidu,
    Google,
    GoogleScholar,
    Github,
    Arxiv,
    SemanticScholar,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum NetworkStatus {
    Reachable,
    Timeout,
    Unreachable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkEndpointDiagnostic {
    id: EndpointId,
    url: String,
    status: NetworkStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkDoctorReport {
    endpoints: Vec<NetworkEndpointDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    proxy: Option<String>,
}

fn truncate_detail(text: &str) -> String {
    let mut chars = text.chars();
    let mut out = String::with_capacity(NETWORK_DETAIL_MAX_CHARS.min(text.len()));
    for _ in 0..NETWORK_DETAIL_MAX_CHARS {
        match chars.next() {
            Some(character) => out.push(character),
            None => break,
        }
    }
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// `doctor_check_network` → `ApiResult<NetworkDoctorReport>`.
///
/// Self-contained port of the desktop `features::system::network` diagnostics:
/// six fixed endpoints probed concurrently through the shared proxy config.
async fn doctor_check_network(_ctx: &RpcCtx, _args: &Value) -> HandlerResult {
    let client = match http::client_with(
        NETWORK_PROBE_TIMEOUT,
        DEFAULT_REDIRECT_LIMIT,
        BROWSER_USER_AGENT,
    ) {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    let endpoints: &[(&'static str, EndpointId, &'static str)] = &[
        ("baidu", EndpointId::Baidu, "https://www.baidu.com/"),
        (
            "google",
            EndpointId::Google,
            "https://www.google.com/generate_204",
        ),
        (
            "google-scholar",
            EndpointId::GoogleScholar,
            "https://scholar.google.com/",
        ),
        ("github", EndpointId::Github, "https://api.github.com/"),
        (
            "arxiv",
            EndpointId::Arxiv,
            "https://export.arxiv.org/api/query?id_list=1706.03762&max_results=1",
        ),
        (
            "semantic-scholar",
            EndpointId::SemanticScholar,
            "https://api.semanticscholar.org/graph/v1/paper/ARXIV:1706.03762?fields=title",
        ),
    ];
    let probes = endpoints.iter().map(|(_, id, url)| {
        let client = client.clone();
        let id = *id;
        let url = *url;
        async move {
            let started = Instant::now();
            match client.get(url).send().await {
                Ok(response) => NetworkEndpointDiagnostic {
                    id,
                    url: url.to_string(),
                    status: NetworkStatus::Reachable,
                    status_code: Some(response.status().as_u16()),
                    latency_ms: Some(started.elapsed().as_millis() as u64),
                    detail: None,
                },
                Err(error) => NetworkEndpointDiagnostic {
                    id,
                    url: url.to_string(),
                    status: if error.is_timeout() {
                        NetworkStatus::Timeout
                    } else {
                        NetworkStatus::Unreachable
                    },
                    status_code: error.status().map(|status| status.as_u16()),
                    latency_ms: Some(started.elapsed().as_millis() as u64),
                    detail: Some(truncate_detail(&error.to_string())),
                },
            }
        }
    });
    let endpoints = join_all(probes).await;
    envelope(ApiResult::ok(NetworkDoctorReport {
        endpoints,
        proxy: http::effective_proxy_url(),
    }))
}

// ---------------------------------------------------------------------------
// vault doctor commands (shared with the desktop Settings → Doctor page)
// ---------------------------------------------------------------------------

static DOCTOR_DIRTY_PATHS: OnceLock<vault_doctor::DoctorDirtyPathsState> = OnceLock::new();

fn doctor_dirty_paths() -> &'static vault_doctor::DoctorDirtyPathsState {
    DOCTOR_DIRTY_PATHS.get_or_init(vault_doctor::DoctorDirtyPathsState::default)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCheckArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoctorApplyAliasesArgs {
    vault_path: String,
    changes: Vec<vault_doctor::AliasRepairChange>,
    #[serde(default)]
    dirty_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoctorSetDirtyPathsArgs {
    vault_path: String,
    #[serde(default)]
    dirty_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoctorIgnoreAliasesArgs {
    vault_path: String,
    #[serde(default)]
    paths: Vec<String>,
    ignore: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoctorPlanWikilinksArgs {
    vault_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoctorApplyWikilinksArgs {
    vault_path: String,
    changes: Vec<vault_doctor::WikilinkRepairChange>,
    #[serde(default)]
    dirty_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoctorApplyVisualMarksArgs {
    vault_path: String,
    changes: Vec<vault_doctor::VisualMarkRepairChange>,
    #[serde(default)]
    dirty_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoctorFixCatalogDuplicatesArgs {
    vault_path: String,
}

/// `doctor_check({args:{vaultPath}})` → `ApiResult<DoctorReport>`.
async fn doctor_check(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: DoctorCheckArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_repair(move || match vault_root(&args.vault_path) {
        Ok(vault) => match vault_doctor::diagnose(&vault) {
            Ok(report) => Ok(report),
            Err(error) => Err((error.to_string(), None)),
        },
        Err(message) => Err((message, None)),
    })
    .await
}

/// `doctor_fix_catalog_duplicates({args:{vaultPath}})` →
/// `ApiResult<DuplicateRepairResult>`.
async fn doctor_fix_catalog_duplicates(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: DoctorFixCatalogDuplicatesArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_repair(move || match vault_root(&args.vault_path) {
        Ok(vault) => match vault_doctor::apply_catalog_duplicate_repairs(&vault) {
            Ok(result) => Ok(result),
            Err(error) => Err((error.to_string(), None)),
        },
        Err(message) => Err((message, None)),
    })
    .await
}

/// `doctor_ignore_aliases({args:{vaultPath, paths, ignore}})` →
/// `ApiResult<DoctorVaultState>`.
async fn doctor_ignore_aliases(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: DoctorIgnoreAliasesArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_repair(move || {
        let vault = PathBuf::from(args.vault_path.trim());
        match vault_doctor::set_ignored_alias_paths(&vault, &args.paths, args.ignore) {
            Ok(state) => Ok(state),
            Err(error) => Err((error.to_string(), None)),
        }
    })
    .await
}

/// `doctor_set_dirty_paths({args:{vaultPath, dirtyPaths}})` →
/// `ApiResult<null>` (in-memory only).
async fn doctor_set_dirty_paths(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: DoctorSetDirtyPathsArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    match doctor_dirty_paths().set(&args.vault_path, &args.dirty_paths) {
        Ok(()) => ok(()),
        Err(error) => fail(AppError::message(format!(
            "doctor dirty paths lock: {error}"
        ))),
    }
}

/// `doctor_plan_wikilinks({args:{vaultPath}})` → `ApiResult<WikilinkRepairPlan>`.
async fn doctor_plan_wikilinks(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: DoctorPlanWikilinksArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    blocking_repair(move || match vault_root(&args.vault_path) {
        Ok(vault) => match vault_doctor::plan_wikilink_repairs(&vault) {
            Ok(plan) => Ok(plan),
            Err(error) => Err((
                error.to_string(),
                Some(serde_json::to_value(&error).unwrap_or(Value::Null)),
            )),
        },
        Err(message) => Err((message, None)),
    })
    .await
}

/// Rebuild the shared wiki index after a repair touched sources.
fn rebuild_wiki_index(vault_path: &str) -> DetailedRepair<()> {
    let mut guard = match lock_wiki_index() {
        Ok(guard) => guard,
        Err(error) => return Err((error, None)),
    };
    match guard.rebuild(vault_path) {
        Ok(_) => Ok(()),
        Err(error) => Err((
            format!("repair applied but Wiki index rebuild failed: {error}"),
            None,
        )),
    }
}

/// `doctor_apply_aliases({args:{vaultPath, changes, dirtyPaths}})` →
/// `ApiResult<AliasRepairResult>`.
async fn doctor_apply_aliases(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: DoctorApplyAliasesArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let mut dirty_paths = match doctor_dirty_paths().get(&args.vault_path) {
        Ok(paths) => paths,
        Err(error) => {
            return fail(AppError::message(format!(
                "doctor dirty paths lock: {error}"
            )))
        }
    };
    blocking_repair(move || {
        dirty_paths.extend(args.dirty_paths.clone());
        let vault = match vault_root(&args.vault_path) {
            Ok(vault) => vault,
            Err(message) => return Err((message, None)),
        };
        match vault_doctor::apply_alias_repairs(&vault, &args.changes, &dirty_paths) {
            Ok(result) => {
                if !result.updated_paths.is_empty() {
                    rebuild_wiki_index(&vault.to_string_lossy())?;
                }
                Ok(result)
            }
            Err(error) => detailed_error(
                &error,
                error.to_string(),
            ),
        }
    })
    .await
}

/// `doctor_apply_wikilinks({args:{vaultPath, changes, dirtyPaths}})` →
/// `ApiResult<WikilinkRepairResult>`.
async fn doctor_apply_wikilinks(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: DoctorApplyWikilinksArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let mut dirty_paths = match doctor_dirty_paths().get(&args.vault_path) {
        Ok(paths) => paths,
        Err(error) => {
            return fail(AppError::message(format!(
                "doctor dirty paths lock: {error}"
            )))
        }
    };
    blocking_repair(move || {
        dirty_paths.extend(args.dirty_paths.clone());
        let vault = match vault_root(&args.vault_path) {
            Ok(vault) => vault,
            Err(message) => return Err((message, None)),
        };
        match vault_doctor::apply_wikilink_repairs(&vault, &args.changes, &dirty_paths) {
            Ok(result) => {
                if !result.updated_paths.is_empty() {
                    rebuild_wiki_index(&vault.to_string_lossy())?;
                }
                Ok(result)
            }
            Err(error) => detailed_error(
                &error,
                error.to_string(),
            ),
        }
    })
    .await
}

/// `doctor_apply_visual_marks({args:{vaultPath, changes, dirtyPaths}})` →
/// `ApiResult<VisualMarkRepairResult>`.
async fn doctor_apply_visual_marks(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let args: DoctorApplyVisualMarksArgs = match parse_args(args) {
        Ok(args) => args,
        Err(error) => return typed_err(error),
    };
    let mut dirty_paths = match doctor_dirty_paths().get(&args.vault_path) {
        Ok(paths) => paths,
        Err(error) => {
            return fail(AppError::message(format!(
                "doctor dirty paths lock: {error}"
            )))
        }
    };
    blocking_repair(move || {
        dirty_paths.extend(args.dirty_paths.clone());
        let vault = match vault_root(&args.vault_path) {
            Ok(vault) => vault,
            Err(message) => return Err((message, None)),
        };
        match vault_doctor::apply_visual_mark_repairs(&vault, &args.changes, &dirty_paths) {
            Ok(result) => Ok(result),
            Err(error) => Err((error.to_string(), None)),
        }
    })
    .await
}

// ---------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------

/// Route one `agent_*` / `doctor_*` / `remote_agent_*` command.
pub async fn dispatch(ctx: &RpcCtx, command: &str, args: &Value) -> Option<HandlerResult> {
    let result = match command {
        // registry / catalog
        "agent_list_agents" => list_agents(ctx, args).await,
        "agent_list_skills" => list_skills(ctx, args).await,
        "agent_scan_catalog" => scan_catalog(ctx, args).await,
        "agent_check_catalog_updates" => check_catalog_updates(ctx, args).await,
        "agent_upsert_agent" => upsert_agent(ctx, args).await,
        "agent_ensure_catalog" => ensure_catalog(ctx, args).await,
        "agent_remove_agent" => remove_agent(ctx, args).await,
        "agent_set_default" => set_default(ctx, args).await,
        "agent_set_user_agent" => set_user_agent(ctx, args).await,
        "agent_probe" => probe(ctx, args).await,
        "agent_probe_catalog" => probe_catalog(ctx, args).await,
        "host_agent_redetect" => host_agent_redetect(ctx).await,

        // sessions
        "agent_run_once" => run_once(ctx, args).await,
        "agent_cancel_run" => cancel_run(ctx, args).await,
        "agent_warm" => warm(ctx, args).await,
        "agent_list_sessions" => list_sessions(ctx, args).await,
        "agent_load_session" => load_session(ctx, args).await,

        // user gates
        "agent_respond_permission" => respond_permission(ctx, args).await,
        "agent_respond_elicitation" => respond_elicitation(ctx, args).await,
        "agent_respond_ask_user" => respond_ask_user(ctx, args).await,

        // citation / lifecycle
        "agent_resolve_citation" => resolve_citation(ctx, args).await,
        "agent_run_tool_lifecycle" => run_tool_lifecycle(ctx, args).await,
        "agent_run_partial_uninstall" => run_partial_uninstall(ctx, args).await,
        "agent_lifecycle_cancel" => lifecycle_cancel(ctx, args).await,
        "agent_tool_uninstall_info" => tool_uninstall_info(ctx, args).await,

        // agent / network doctor
        "doctor_check_host" => doctor_check_host(ctx, args).await,
        "doctor_install_node" => doctor_install_node(ctx, args).await,
        "doctor_check_agents" => doctor_check_agents(ctx, args).await,
        "doctor_check_network" => doctor_check_network(ctx, args).await,
        "doctor_open_agent_login_terminal" => unsupported(
            "opening a terminal for the agent login flow is not available in the headless web host",
        ),
        "doctor_open_agent_cli_terminal" => unsupported(
            "opening a terminal for the agent CLI is not available in the headless web host",
        ),

        // vault doctor
        "doctor_check" => doctor_check(ctx, args).await,
        "doctor_fix_catalog_duplicates" => doctor_fix_catalog_duplicates(ctx, args).await,
        "doctor_ignore_aliases" => doctor_ignore_aliases(ctx, args).await,
        "doctor_set_dirty_paths" => doctor_set_dirty_paths(ctx, args).await,
        "doctor_plan_wikilinks" => doctor_plan_wikilinks(ctx, args).await,
        "doctor_apply_aliases" => doctor_apply_aliases(ctx, args).await,
        "doctor_apply_wikilinks" => doctor_apply_wikilinks(ctx, args).await,
        "doctor_apply_visual_marks" => doctor_apply_visual_marks(ctx, args).await,

        // Remote-vault surface: no remote registry in the headless host.
        "remote_agent_scan" | "remote_agent_probe" | "remote_agent_open_install_terminal" => {
            unsupported("remote-vault agents are not available in the headless web host")
        }

        _ => return None,
    };
    Some(result)
}
