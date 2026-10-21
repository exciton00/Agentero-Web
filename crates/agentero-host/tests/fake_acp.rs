//! End-to-end ACP tests against a fake stdio agent (`fixtures/fake_acp_agent.js`).
//!
//! These prove the ported stack works without Tauri:
//! spawn → `initialize` → `session/new` → `session/prompt` stream → cancel,
//! with the real `agent:*` event contract emitted through `HostSink`.

use agentero_host::acp::session::{run_prompt_turn, PromptTurn, TurnPolicy};
use agentero_host::host::{MockHost, MockSink};
use agentero_host::models::{AgentDescriptor, AgentTemplate};
use agentero_host::models::{RunOnceRequest, UpsertAgentRequest};
use agentero_host::registry::AgentRegistry;
use agentero_host::AgentHost;
use agentero_host::{probe_agent, PermissionPolicy};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn fake_agent_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("fake_acp_agent.js");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_acp_agent.js"),
        &script,
    )
    .expect("copy fake agent");
    (dir, script)
}

fn descriptor(script: &Path, env: HashMap<String, String>) -> AgentDescriptor {
    AgentDescriptor {
        id: "fake".to_string(),
        name: "Fake ACP".to_string(),
        template: AgentTemplate::Custom,
        command: "node".to_string(),
        args: vec![script.to_string_lossy().into_owned()],
        env,
        available: true,
        last_error: None,
        last_probe_ok: None,
        last_probe_agent_name: None,
        last_probe_error: None,
        last_probed_at: None,
    }
}

fn prompt_turn() -> PromptTurn {
    PromptTurn {
        session_id: "run-1".to_string(),
        message_id: "msg-1".to_string(),
        prompt: "hello".to_string(),
        images: Vec::new(),
        resume_session_id: None,
        provider_agent_id: "fake".to_string(),
    }
}

#[tokio::test]
async fn prompt_turn_streams_updates_and_completes() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let (_script_dir, script) = fake_agent_dir();
    let vault = tempfile::tempdir().expect("vault dir");
    let desc = descriptor(&script, HashMap::new());
    let sink = Arc::new(MockSink::new());

    let outcome = run_prompt_turn(
        sink.clone(),
        Some("tab-1".to_string()),
        &desc,
        Some(vault.path().to_str().unwrap()),
        prompt_turn(),
        TurnPolicy::restricted(),
    )
    .await
    .expect("turn succeeds");

    assert_eq!(outcome.content, "Hello");
    assert_eq!(
        outcome.reasoning.as_deref(),
        Some("thinking..."),
        "thought chunks are buffered separately"
    );
    assert_eq!(outcome.provider_session_id, "fake-session-1");
    assert_eq!(outcome.stop_reason.as_deref(), Some("EndTurn"));
    assert!(!outcome.cancelled);

    let events = sink.events();
    let names = sink.event_names();
    assert_eq!(names.first().map(String::as_str), Some("agent:status"));
    assert!(names.contains(&"agent:stream".to_string()));
    assert!(names.contains(&"agent:tool".to_string()));
    assert!(names.contains(&"agent:usage".to_string()));
    assert_eq!(names.last().map(String::as_str), Some("agent:completed"));
    assert!(
        events
            .iter()
            .all(|event| event.target.as_deref() == Some("tab-1")),
        "targeted events must carry the window label: {events:?}"
    );

    let completed = sink.find("agent:completed").expect("completed event");
    assert_eq!(completed.payload["content"], "Hello");
    assert_eq!(completed.payload["providerSessionId"], "fake-session-1");
    assert_eq!(completed.payload["stopReason"], "EndTurn");

    let tool = sink.find("agent:tool").expect("tool event");
    assert_eq!(tool.payload["toolCallId"], "tool-1");
    assert_eq!(tool.payload["status"], "completed");
    assert_eq!(tool.payload["title"], "Read NOTES.md");

    let usage = sink.find("agent:usage").expect("usage event");
    assert_eq!(usage.payload["used"], 12);
    assert_eq!(usage.payload["size"], 1000);

    // `waiting-model` lands before the first streamed chunk.
    let waiting = events
        .iter()
        .position(|event| {
            event.event == "agent:status" && event.payload["phase"] == "waiting-model"
        })
        .expect("waiting-model status");
    let first_stream = events
        .iter()
        .position(|event| event.event == "agent:stream")
        .expect("stream event");
    assert!(waiting < first_stream);
}

#[tokio::test]
async fn prompt_turn_cancels_cooperatively() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let (_script_dir, script) = fake_agent_dir();
    let vault = tempfile::tempdir().expect("vault dir");
    let mut env = HashMap::new();
    env.insert("FAKE_ACP_MODE".to_string(), "cancel".to_string());
    let desc = descriptor(&script, env);
    let sink = Arc::new(MockSink::new());

    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = tx.send(true);
    });

    let outcome = run_prompt_turn(
        sink.clone(),
        Some("tab-2".to_string()),
        &desc,
        Some(vault.path().to_str().unwrap()),
        prompt_turn(),
        TurnPolicy::with_cancellation(PermissionPolicy::Restricted, Default::default(), rx),
    )
    .await
    .expect("cancelled turn is not an error");

    assert!(outcome.cancelled);
    assert_eq!(outcome.stop_reason.as_deref(), Some("cancelled"));
    let completed = sink.find("agent:completed").expect("completed event");
    assert_eq!(completed.payload["stopReason"], "cancelled");
    assert_eq!(completed.payload["sessionId"], "run-1");
}

#[tokio::test]
async fn probe_reports_the_fake_agent_identity() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let (_script_dir, script) = fake_agent_dir();
    let desc = descriptor(&script, HashMap::new());
    let result = probe_agent(&desc, None).await;
    assert!(result.available, "probe failed: {:?}", result.error);
    assert_eq!(result.agent_name.as_deref(), Some("fake-acp"));
    assert_eq!(
        result.protocol_version.as_deref(),
        Some("ProtocolVersion(1)")
    );
    let caps = result.session_capabilities.expect("session caps");
    assert!(caps.resume);
    assert!(caps.load);
    assert!(caps.list);
    assert!(caps.delete);
}

#[tokio::test]
async fn list_and_load_sessions_via_acp() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let (_script_dir, script) = fake_agent_dir();
    let vault = tempfile::tempdir().expect("vault dir");
    let desc = descriptor(&script, HashMap::new());
    let cwd = vault.path().to_path_buf();

    let list = agentero_host::list_acp_sessions(&desc, cwd.clone(), None, None)
        .await
        .expect("session/list");
    assert!(list.supported);
    assert_eq!(list.sessions.len(), 1);
    assert_eq!(list.sessions[0].session_id, "fake-session-1");
    assert_eq!(list.sessions[0].title.as_deref(), Some("Fake session"));

    let load = agentero_host::load_acp_session(&desc, "fake-session-1".to_string(), cwd, None)
        .await
        .expect("session/load");
    assert_eq!(load.session_id, "fake-session-1");
    assert!(load.lines.is_empty());
}

#[tokio::test]
async fn service_run_once_streams_through_the_host_sink() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let (_script_dir, script) = fake_agent_dir();
    let vault = tempfile::tempdir().expect("vault dir");
    let host = Arc::new(MockHost::new());
    let service = AgentHost::new(host.clone());

    let agent = service
        .upsert_agent(UpsertAgentRequest {
            id: None,
            name: "Fake ACP".to_string(),
            template: Some(AgentTemplate::Custom),
            command: "node".to_string(),
            args: vec![script.to_string_lossy().into_owned()],
            env: HashMap::new(),
            set_default: true,
        })
        .expect("register fake agent");

    let accepted = service
        .accept_run_once(
            Some("tab-9".to_string()),
            RunOnceRequest {
                agent_id: Some(agent.agent.id.clone()),
                session_id: None,
                prompt: "hello".to_string(),
                is_acp_command: false,
                images: Vec::new(),
                vault_path: Some(vault.path().to_string_lossy().into_owned()),
                workflow: None,
                target: None,
                model_id: None,
                collaboration_mode_id: None,
                reasoning_effort: None,
                prefer_highest_reasoning_effort: false,
                fast_mode: None,
                skill_ids: Vec::new(),
                auto_approve: false,
                permission_mode: None,
                response_language: None,
                personal_prompt: None,
                hide_from_chat_history: false,
                context: None,
                resume_transcript: None,
            },
        )
        .await
        .expect("accept run");

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if host
            .events()
            .iter()
            .any(|event| event.event == "agent:completed")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "turn did not finish: {:?}",
            host.events()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let events = host.events();
    let completed = events
        .iter()
        .find(|event| event.event == "agent:completed")
        .expect("completed");
    assert_eq!(completed.payload["sessionId"], accepted.session_id);
    assert_eq!(completed.payload["content"], "Hello");
    assert_eq!(completed.target.as_deref(), Some("tab-9"));
    assert!(events
        .iter()
        .any(|event| event.event == "agent:stream" && event.target.as_deref() == Some("tab-9")));
    assert!(!events.iter().any(|event| event.event == "agent:failed"));
}

#[test]
fn registry_persists_under_the_host_config_dir() {
    let dir = tempfile::tempdir().expect("config dir");
    let registry = AgentRegistry::load_in(dir.path());
    let state = registry.snapshot().expect("snapshot");
    assert!(state.enabled);
    assert!(state.agents.is_empty());

    let agent = registry
        .upsert(agentero_host::models::UpsertAgentRequest {
            id: None,
            name: "My Agent".to_string(),
            template: Some(AgentTemplate::Custom),
            command: "node".to_string(),
            args: vec!["acp".to_string()],
            env: HashMap::new(),
            set_default: true,
        })
        .expect("upsert");
    assert_eq!(agent.name, "My Agent");
    assert!(dir.path().join("agents.json").is_file());

    let state = registry.snapshot().expect("snapshot");
    assert_eq!(state.agents.len(), 1);
    assert_eq!(state.default_id.as_deref(), Some(agent.id.as_str()));
    let resolved = registry.resolve_default(None).expect("default");
    assert_eq!(resolved.id, agent.id);

    // A fresh registry on the same directory sees the persisted agent.
    let reloaded = AgentRegistry::load_in(dir.path());
    assert_eq!(reloaded.snapshot().expect("snapshot").agents.len(), 1);
}

#[test]
fn catalog_exposes_every_builtin_template() {
    let templates = agentero_host::catalog_templates();
    assert!(templates.len() >= 11, "catalog: {templates:?}");
    let ids: Vec<&str> = templates.iter().map(|entry| entry.id.as_str()).collect();
    for expected in [
        "opencode",
        "claude-acp",
        "codex-acp",
        "hermes",
        "qodercli",
        "grok-build",
        "pi",
        "dsh",
        "kimi-code",
        "zcode",
        "minimax-code",
        "mimo-code",
    ] {
        assert!(ids.contains(&expected), "missing template {expected}");
    }
}
