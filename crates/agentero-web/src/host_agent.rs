//! Auto-registration for an ACP agent provided by the Docker host.
//!
//! A container cannot see the host filesystem, so `scripts/docker-up.sh` (or
//! the host-agent compose override) bind-mounts the host's Dsh package and
//! `~/.dsh`. This module detects those mounts and registers a custom agent
//! through the same registry the UI uses, so no manual "add agent" step is
//! needed. It deliberately only acts when both the CLI entry and its config
//! directory are present.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agentero_host::models::UpsertAgentRequest;
use agentero_host::service::AgentHost;

/// Standard mount point used by `docker-compose.host-agent.yml.example`.
const MOUNTED_DSH_ENTRY: &str = "/opt/host-agent/@deepseek-ai/dsh/lib/bin.js";
/// Alternative layout used by an image that installs Dsh globally.
const IMAGE_DSH_ENTRY: &str = "/usr/lib/node_modules/@deepseek-ai/dsh/lib/bin.js";
const HOST_AGENT_ID: &str = "host-dsh";
/// Written after the first auto-registration (or when a matching agent already
/// exists). If the user removes the agent, the marker keeps it removed on
/// restart; delete the marker to opt back in.
const AUTO_MARKER: &str = ".host-agent-auto.flag";

fn entry_from_env() -> Option<PathBuf> {
    std::env::var_os("AGENTERO_HOST_DSH_ENTRY").map(PathBuf::from)
}

fn first_existing(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

fn node_binary() -> String {
    if let Ok(node) = std::env::var("AGENTERO_NODE_BIN") {
        if !node.trim().is_empty() {
            return node;
        }
    }
    if Path::new("/usr/local/bin/node").is_file() {
        "/usr/local/bin/node".to_string()
    } else {
        "node".to_string()
    }
}

/// Register the host-provided Dsh agent when its mount and config are present.
///
/// Returns `true` when an agent was registered by this call. Errors are logged
/// and otherwise ignored: a missing/invalid host agent must never prevent the
/// web host from starting.
pub fn ensure_host_dsh(agents: &AgentHost, settings_path: &Path) -> bool {
    let marker = settings_path.with_file_name(AUTO_MARKER);
    if marker.is_file() {
        return false;
    }
    if std::env::var("AGENTERO_HOST_DSH_AUTO")
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off"))
        .unwrap_or(false)
    {
        return false;
    }

    let Some(entry) = entry_from_env().or_else(|| {
        first_existing(&[MOUNTED_DSH_ENTRY, IMAGE_DSH_ENTRY])
    }) else {
        return false;
    };
    if !entry.is_file() {
        log::warn!(
            "host-agent: Dsh entry {} does not exist; skipping auto-registration",
            entry.display()
        );
        return false;
    }

    let home = std::env::var("DSH_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(|home| Path::new(&home).join(".dsh"))
                .unwrap_or_else(|_| PathBuf::from("/data/home/.dsh"))
        });
    if !home.is_dir() {
        log::warn!(
            "host-agent: Dsh home {} is missing (mount the host ~/.dsh); skipping auto-registration",
            home.display()
        );
        return false;
    }

    let command = node_binary();
    let args = vec![
        entry.to_string_lossy().into_owned(),
        "--profile".to_string(),
        "acp".to_string(),
    ];

    let mark_done = || {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(&marker, b"registered\n") {
            log::warn!("host-agent: could not write {}: {error}", marker.display());
        }
    };

    if let Ok(list) = agents.list_agents() {
        let already = list.agents.iter().any(|agent| {
            agent.command == command
                && agent.args.iter().any(|arg| arg == &args[0])
        });
        if already {
            mark_done();
            return false;
        }
    }

    let request = UpsertAgentRequest {
        id: Some(HOST_AGENT_ID.to_string()),
        name: "Host Dsh".to_string(),
        template: None,
        command,
        args,
        env: HashMap::new(),
        set_default: true,
    };
    match agents.upsert_agent(request) {
        Ok(agent) => {
            mark_done();
            log::info!(
                "host-agent: registered {} (Dsh at {})",
                agent.agent.name,
                home.display()
            );
            true
        }
        Err(error) => {
            log::warn!("host-agent: could not register host Dsh: {error}");
            false
        }
    }
}
