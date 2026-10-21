//! Host-facing service facade for the agent domain.
//!
//! Replaces the desktop `features::agent::service` + `#[tauri::command]` shells:
//! one owned [`AgentHost`] holds the registry, run controller, gates and warm
//! pool, and exposes plain `async fn`s for the RPC layer. No Tauri state
//! container, no `WebviewWindow`: events go through [`HostSink`] with an
//! optional target label.
//!
//! `agentero-web` maps each RPC command to one of these methods 1:1.

use crate::acp::client::agent_spawn_cwd;
use crate::acp::interaction::PermissionPolicy;
use crate::acp::probe_agent;
use crate::host::Host;
use crate::models::{
    AgentDescriptor, AgentFailedEvent, AgentListResponse, AgentOnly, AgentSkill, AgentStatusEvent,
    AskUserResponseRequest, CatalogScanResponse, ElicitationResponseRequest, PermissionResponded,
    PermissionResponseRequest, ProbeResult, RunOnceAccepted, RunOnceRequest, UpsertAgentRequest,
    WarmRequest, WarmResult,
};
use crate::registry::{version_check, AgentRegistry};
use crate::runtime::events::AgentEventEmitter;
use crate::runtime::gates::{
    AskUserAnswer, AskUserGate, ElicitationAnswer, ElicitationGate, PermissionGate,
};
use crate::runtime::{AgentRunController, AgentWarmGate};
use crate::session::{
    list_acp_sessions, load_acp_session, new_ids, run_once, warm_agent, AgentWarmPool,
    RunOnceParams,
};
use agentero_core::error::AppError;
use std::path::Path;
use std::sync::Arc;

/// One agent domain instance for a host process.
pub struct AgentHost {
    host: Arc<dyn Host>,
    registry: AgentRegistry,
    runs: Arc<AgentRunController>,
    permission_gate: PermissionGate,
    elicitation_gate: ElicitationGate,
    ask_user_gate: AskUserGate,
    warm_gate: Arc<AgentWarmGate>,
    warm_pool: Arc<AgentWarmPool>,
}

impl AgentHost {
    /// Load `agents.json` from `host.config_dir()` and wire the bundled adapters
    /// from `host.resource_dir()`.
    pub fn new(host: Arc<dyn Host>) -> Self {
        crate::registry::bundled::init(host.resource_dir());
        let registry = AgentRegistry::load_in(host.config_dir());
        Self::with_registry(host, registry)
    }

    /// Use a caller-provided registry (tests, pre-opened config dir).
    pub fn with_registry(host: Arc<dyn Host>, registry: AgentRegistry) -> Self {
        Self {
            host,
            registry,
            runs: Arc::new(AgentRunController::new()),
            permission_gate: PermissionGate::new(),
            elicitation_gate: ElicitationGate::new(),
            ask_user_gate: AskUserGate::new(),
            warm_gate: Arc::new(AgentWarmGate::new()),
            warm_pool: Arc::new(AgentWarmPool::new()),
        }
    }

    pub fn host(&self) -> &Arc<dyn Host> {
        &self.host
    }

    pub fn registry(&self) -> &AgentRegistry {
        &self.registry
    }

    pub fn permission_gate(&self) -> &PermissionGate {
        &self.permission_gate
    }

    pub fn elicitation_gate(&self) -> &ElicitationGate {
        &self.elicitation_gate
    }

    pub fn ask_user_gate(&self) -> &AskUserGate {
        &self.ask_user_gate
    }

    pub fn warm_pool(&self) -> &Arc<AgentWarmPool> {
        &self.warm_pool
    }

    fn emitter(&self, target: Option<String>) -> AgentEventEmitter {
        AgentEventEmitter::with_target(self.host.sink_arc(), target)
    }

    /// Mounted Agent panels refresh on registry mutations.
    pub fn emit_registry_changed(&self) {
        AgentEventEmitter::broadcast(self.host.sink_arc())
            .emit("agent:registry-changed", serde_json::json!({}));
    }

    // ---- registry / catalog -------------------------------------------------

    pub fn list_agents(&self) -> Result<AgentListResponse, AppError> {
        let state = self.registry.snapshot()?;
        Ok(AgentListResponse {
            agents: state.agents,
            default_id: state.default_id,
            enabled: state.enabled,
        })
    }

    pub fn list_skills(&self, vault_path: Option<&str>) -> Vec<AgentSkill> {
        crate::prompt::skills::list_agent_skills(vault_path)
    }

    pub fn scan_catalog(&self) -> Result<CatalogScanResponse, AppError> {
        self.registry.scan_catalog()
    }

    /// Scan + compare installed versions with npm latest (blocking I/O off the
    /// async runtime).
    pub async fn check_catalog_updates(&self) -> Result<CatalogScanResponse, AppError> {
        let mut scan = self.registry.scan_catalog()?;
        let (proxy_enabled, proxy_url) = self.registry.proxy_settings().unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            version_check::enrich_catalog_updates(&mut scan, proxy_enabled, &proxy_url);
            scan
        })
        .await
        .map_err(|error| AppError::message(format!("catalog update check join error: {error}")))
    }

    pub fn upsert_agent(&self, request: UpsertAgentRequest) -> Result<AgentOnly, AppError> {
        let agent = self.registry.upsert(request)?;
        self.emit_registry_changed();
        Ok(AgentOnly { agent })
    }

    pub fn ensure_catalog(
        &self,
        template_id: &str,
        set_default: bool,
    ) -> Result<AgentOnly, AppError> {
        let agent = self
            .registry
            .ensure_catalog_agent(template_id, set_default)?;
        self.emit_registry_changed();
        Ok(AgentOnly { agent })
    }

    pub fn remove_agent(&self, id: &str) -> Result<(), AppError> {
        self.registry.remove(id)?;
        self.emit_registry_changed();
        Ok(())
    }

    pub fn set_default(&self, id: Option<String>) -> Result<AgentListResponse, AppError> {
        let state = self.registry.set_default(id)?;
        self.emit_registry_changed();
        Ok(AgentListResponse {
            agents: state.agents,
            default_id: state.default_id,
            enabled: state.enabled,
        })
    }

    pub fn set_user_agent(
        &self,
        user_agent: String,
        user_agent_provider_ids: String,
    ) -> Result<AgentListResponse, AppError> {
        let state = self
            .registry
            .set_user_agent(user_agent, user_agent_provider_ids)?;
        Ok(AgentListResponse {
            agents: state.agents,
            default_id: state.default_id,
            enabled: state.enabled,
        })
    }

    pub async fn probe(&self, id: &str) -> Result<ProbeResult, AppError> {
        let desc = self.registry.get(id)?;
        let result = probe_agent(&desc, None).await;
        let _ = self.registry.apply_probe_result(&desc.id, &result);
        self.emit_registry_changed();
        Ok(result)
    }

    /// Ensure the catalog template is registered, then ACP-probe it.
    pub async fn probe_catalog(&self, template_id: &str) -> Result<ProbeResult, AppError> {
        let desc = self.registry.ensure_catalog_agent(template_id, false)?;
        if !desc.available {
            let result = ProbeResult {
                agent_id: desc.id.clone(),
                available: false,
                agent_name: None,
                protocol_version: None,
                error: desc
                    .last_error
                    .clone()
                    .or_else(|| Some(format!("command `{}` not found on PATH", desc.command))),
                session_capabilities: None,
            };
            let _ = self.registry.apply_probe_result(&desc.id, &result);
            self.emit_registry_changed();
            return Ok(result);
        }
        let result = probe_agent(&desc, None).await;
        let _ = self.registry.apply_probe_result(&desc.id, &result);
        self.emit_registry_changed();
        Ok(result)
    }

    // ---- sessions -----------------------------------------------------------

    /// Validate a prompt, register the run, and stream the turn in the
    /// background. Events for the turn target `target` (tab/window label).
    pub async fn accept_run_once(
        &self,
        target: Option<String>,
        request: RunOnceRequest,
    ) -> Result<RunOnceAccepted, AppError> {
        if request.prompt.trim().is_empty() && request.images.is_empty() {
            return Err(AppError::message("prompt or images are required"));
        }
        let desc = self.registry.resolve_default(request.agent_id.as_deref())?;
        let (session_id, message_id) = new_ids();
        let accepted = RunOnceAccepted {
            session_id: session_id.clone(),
            message_id: message_id.clone(),
            agent_id: desc.id.clone(),
        };
        let cancellation = self.runs.register(&session_id)?;
        let events = self.emitter(target);
        // `starting` lands before the accepted ids return, so the UI can paint
        // a loading state at t=0.
        events.emit(
            "agent:status",
            AgentStatusEvent::starting(session_id.as_str()),
        );

        let permission_policy = match request.permission_mode.as_deref() {
            Some("auto") => PermissionPolicy::Auto,
            Some("ask") => PermissionPolicy::Ask,
            _ if request.auto_approve => PermissionPolicy::Auto,
            _ => PermissionPolicy::Restricted,
        };
        let permission_gate = self.permission_gate.clone();
        let elicitation_gate = self.elicitation_gate.clone();
        let ask_user_gate = self.ask_user_gate.clone();
        let warm_pool = self.warm_pool.clone();
        let warm_gate = self.warm_gate.clone();
        let runs = self.runs.clone();
        let agent_id = desc.id.clone();
        let log_session = session_id.clone();

        self.host.spawn(Box::pin(async move {
            let result = run_once(RunOnceParams {
                app: events,
                desc,
                session_id: session_id.clone(),
                message_id,
                prompt: request.prompt,
                is_acp_command: request.is_acp_command,
                images: request.images,
                workflow: request.workflow,
                target: request.target,
                vault_path: request.vault_path,
                preferred_model_id: request.model_id,
                preferred_collaboration_mode_id: request.collaboration_mode_id,
                preferred_reasoning_effort: request.reasoning_effort,
                prefer_highest_reasoning_effort: request.prefer_highest_reasoning_effort,
                fast_mode: request.fast_mode,
                skill_ids: request.skill_ids,
                permission_policy,
                permission_gate,
                elicitation_gate,
                ask_user_gate,
                response_language: request.response_language,
                personal_prompt: request.personal_prompt,
                cancellation,
                remote: None,
                resume_session_id: request.session_id,
                warm_pool: Some(warm_pool),
                context: request.context,
                resume_transcript: request.resume_transcript,
            })
            .await;
            if result.is_ok() {
                warm_gate.clear(&agent_id);
            }
            let _ = runs.finish(&session_id);
            log::info!(
                target: "agentero::agent",
                "run finished session={} agent={} ok={}",
                log_session,
                agent_id,
                result.is_ok()
            );
        }));

        Ok(accepted)
    }

    /// Cooperative cancel for a streaming session.
    pub fn cancel_run(&self, session_id: &str) -> Result<bool, AppError> {
        self.runs.cancel(session_id)?;
        Ok(true)
    }

    /// Background ACP warm-up (no prompt) so Chat can show models early.
    pub async fn warm(&self, target: Option<String>, request: WarmRequest) -> WarmResult {
        let desc = match self.registry.resolve_default(request.agent_id.as_deref()) {
            Ok(desc) => desc,
            Err(error) => {
                return WarmResult {
                    agent_id: request.agent_id.unwrap_or_default(),
                    ok: false,
                    models: None,
                    usage_used: None,
                    usage_size: None,
                    error: Some(error.to_string()),
                }
            }
        };
        if let Some(error) = self.warm_gate.blocked(&desc.id) {
            return WarmResult {
                agent_id: desc.id,
                ok: false,
                models: None,
                usage_used: None,
                usage_size: None,
                error: Some(error),
            };
        }
        let agent_id = desc.id.clone();
        let result = warm_agent(
            self.emitter(target),
            desc,
            request.vault_path,
            request.model_id,
            request.collaboration_mode_id,
            None,
            self.warm_pool.clone(),
        )
        .await;
        if result.ok {
            self.warm_gate.clear(&agent_id);
        } else if result.error.as_deref() != Some(crate::session::warm::WARM_CANCELLED_ERROR) {
            self.warm_gate.record_failure(
                &agent_id,
                result.error.as_deref().unwrap_or("agent warm failed"),
            );
        }
        result
    }

    pub async fn list_sessions(
        &self,
        agent_id: Option<String>,
        vault_path: Option<String>,
        cursor: Option<String>,
    ) -> Result<crate::models::AcpListSessionsResult, AppError> {
        let desc = self.registry.resolve_default(agent_id.as_deref())?;
        if let Some(error) = self.warm_gate.blocked(&desc.id) {
            return Err(AppError::message(error));
        }
        let cwd = agent_spawn_cwd(None, vault_path.as_deref())?;
        match list_acp_sessions(&desc, cwd, cursor, None).await {
            Ok(result) => {
                self.warm_gate.clear(&desc.id);
                Ok(result)
            }
            Err(error) => {
                self.warm_gate.record_failure(&desc.id, &error.to_string());
                Err(error)
            }
        }
    }

    pub async fn load_session(
        &self,
        agent_id: Option<String>,
        session_id: String,
        vault_path: Option<String>,
    ) -> Result<crate::models::AcpLoadSessionResult, AppError> {
        let desc = self.registry.resolve_default(agent_id.as_deref())?;
        let cwd = agent_spawn_cwd(None, vault_path.as_deref())?;
        load_acp_session(&desc, session_id, cwd, None).await
    }

    // ---- user gates ---------------------------------------------------------

    pub fn respond_permission(&self, request: PermissionResponseRequest) -> PermissionResponded {
        let resolved = self
            .permission_gate
            .resolve(&request.request_id, request.option_id);
        PermissionResponded { resolved }
    }

    pub fn respond_elicitation(&self, request: ElicitationResponseRequest) -> PermissionResponded {
        let answer = match request.action.as_str() {
            "accept" => ElicitationAnswer::Accept(request.content.unwrap_or_default()),
            "decline" => ElicitationAnswer::Decline,
            _ => ElicitationAnswer::Cancel,
        };
        let resolved = self.elicitation_gate.resolve(&request.request_id, answer);
        PermissionResponded { resolved }
    }

    pub fn respond_ask_user(&self, request: AskUserResponseRequest) -> PermissionResponded {
        let answer = match request.action.as_str() {
            "accept" => AskUserAnswer::Accepted {
                answers: request.answers.unwrap_or_default(),
            },
            _ => AskUserAnswer::Cancelled,
        };
        let resolved = self.ask_user_gate.resolve(&request.request_id, answer);
        PermissionResponded { resolved }
    }

    // ---- doctor / citation --------------------------------------------------

    pub async fn doctor_check_host(&self) -> Result<crate::doctor::HostDoctorReport, AppError> {
        crate::doctor::diagnose_host(&self.registry).await
    }

    pub async fn doctor_install_node(
        &self,
        task_id: Option<&str>,
    ) -> Result<crate::doctor::NodeInstallResult, AppError> {
        let result = crate::doctor::install_node(
            &self.registry,
            Some(self.host.as_ref() as &dyn crate::host::HostSink),
            task_id,
        )
        .await;
        if let Some(task_id) = task_id {
            crate::registry::lifecycle::clear_lifecycle_cancel(task_id);
        }
        result
    }

    pub async fn doctor_check_agents(
        &self,
    ) -> Result<Vec<crate::doctor_agents::AgentAcpDiagnostic>, AppError> {
        crate::doctor_agents::diagnose_agents(
            &self.registry,
            &self.warm_gate,
            self.host.as_ref() as &dyn crate::host::HostSink,
        )
        .await
    }

    /// Resolve a vault-relative citation link to PDF coordinates.
    /// Remote vaults are not supported (web v1).
    pub fn resolve_citation(
        &self,
        vault_path: Option<&str>,
        source: &str,
    ) -> Result<crate::citation::CitationTarget, AppError> {
        let vault = vault_path
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_dir())
            .ok_or_else(|| {
                AppError::domain(
                    "citation_vault_unavailable",
                    "a valid local vault is required to resolve citations",
                )
            })?;
        crate::citation::resolve_citation(&vault, source)
    }

    // ---- desktop-only surface (explicitly unsupported on the web host) ------

    /// Silent install / update / uninstall (`agent_run_tool_lifecycle`).
    /// Blocking (npm / official installers); call from `spawn_blocking`.
    pub fn run_tool_lifecycle(
        &self,
        template_id: &str,
        action: &str,
        task_id: Option<&str>,
    ) -> Result<(), AppError> {
        use crate::registry::lifecycle::{self, ToolLifecycleAction};
        let action = ToolLifecycleAction::parse(action).map_err(AppError::message)?;
        let (proxy_enabled, proxy_url) = self.registry.proxy_settings().unwrap_or_default();
        let result = lifecycle::run_template_lifecycle(
            template_id,
            action,
            Some(self.host.as_ref() as &dyn crate::host::HostSink),
            task_id,
            proxy_enabled,
            &proxy_url,
        )
        .map_err(AppError::message);
        if let Some(task_id) = task_id {
            lifecycle::clear_lifecycle_cancel(task_id);
        }
        result
    }

    /// Uninstall one layer (`agent_run_partial_uninstall`). Removing the host
    /// binary or everything also drops the stale registry entry.
    pub fn run_partial_uninstall(
        &self,
        template_id: &str,
        scope: &str,
        task_id: Option<&str>,
    ) -> Result<(), AppError> {
        use crate::registry::lifecycle::{self, UninstallScope};
        let scope = UninstallScope::parse(scope).map_err(AppError::message)?;
        let (proxy_enabled, proxy_url) = self.registry.proxy_settings().unwrap_or_default();
        let result = lifecycle::run_partial_template_uninstall(
            template_id,
            scope,
            Some(self.host.as_ref() as &dyn crate::host::HostSink),
            task_id,
            proxy_enabled,
            &proxy_url,
        )
        .map_err(AppError::message);
        if let Some(task_id) = task_id {
            lifecycle::clear_lifecycle_cancel(task_id);
        }
        result?;
        if matches!(scope, UninstallScope::Agent | UninstallScope::All) {
            self.registry.remove_catalog_template(template_id)?;
        }
        self.emit_registry_changed();
        Ok(())
    }

    /// Managed uninstall surface for a catalog template.
    pub fn tool_uninstall_info(
        &self,
        template_id: &str,
    ) -> Option<crate::registry::lifecycle::UninstallInfo> {
        crate::registry::lifecycle::uninstall_info(template_id)
    }

    /// Cancel an in-flight install/update/uninstall run.
    pub fn cancel_tool_lifecycle(&self, task_id: &str) -> bool {
        crate::registry::lifecycle::request_lifecycle_cancel(task_id);
        true
    }
}

/// Escape hatch for tests / diagnostics: a failed run emits `agent:failed`
/// even when the caller drops the result.
pub fn emit_failed(
    sink: Arc<dyn crate::host::HostSink>,
    target: Option<String>,
    session_id: &str,
    error: &str,
) {
    AgentEventEmitter::with_target(sink, target).emit(
        "agent:failed",
        AgentFailedEvent {
            session_id: session_id.to_string(),
            error: error.to_string(),
        },
    );
}

/// Convenience: resolve a descriptor without probing (used by RPC preflight).
pub fn descriptor_for(
    registry: &AgentRegistry,
    agent_id: Option<&str>,
) -> Result<AgentDescriptor, AppError> {
    registry.resolve_default(agent_id)
}

/// Ensure the bundled-adapter manifest is loaded from an explicit root. The
/// server uses this at startup when adapters are mounted outside the binary.
pub fn init_adapters(resource_dir: impl AsRef<Path>) {
    crate::registry::bundled::init(Some(resource_dir.as_ref().to_path_buf()));
}
