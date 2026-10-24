//! Auto-registration for a local or host-provided ACP agent.
//!
//! Two deployments are supported:
//! - Docker: `scripts/docker-up.sh` bind-mounts the host Dsh package and
//!   `~/.dsh`; this module finds the mounted `bin.js`.
//! - Source / bare-metal: Dsh is installed on this machine, so `dsh` is on
//!   PATH and `~/.dsh` exists. This also covers the user's own computer.
//!
//! The agent is registered through the same registry as the UI, with the Dsh
//! template so it appears as the normal Dsh row rather than a custom "Host Dsh".

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agentero_host::models::{AgentTemplate, UpsertAgentRequest};
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
const HOST_MARKER_ENV: &str = "AGENTERO_HOST_AGENT";

enum DshSource {
    /// Host package mounted into the container: run it with the image's Node.
    Mounted(PathBuf),
    /// Dsh installed on this machine and available on PATH.
    Path,
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

fn dsh_home() -> PathBuf {
    if let Ok(home) = std::env::var("DSH_HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home);
        }
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".dsh");
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return Path::new(&home).join(".dsh");
        }
    }
    PathBuf::from("/data/home/.dsh")
}

fn dsh_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .map(|ext| ext.trim().to_string())
            .filter(|ext| !ext.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    for dir in std::env::split_paths(&path) {
        if exts.is_empty() {
            if dir.join("dsh").is_file() {
                return true;
            }
        } else {
            for ext in &exts {
                if dir.join(format!("dsh{ext}")).is_file() {
                    return true;
                }
            }
        }
    }
    false
}

fn detect_source() -> Option<DshSource> {
    if let Ok(entry) = std::env::var("AGENTERO_HOST_DSH_ENTRY") {
        let entry = PathBuf::from(entry);
        if entry.is_file() {
            return Some(DshSource::Mounted(entry));
        }
        log::warn!(
            "host-agent: AGENTERO_HOST_DSH_ENTRY={} is not a file; trying PATH",
            entry.display()
        );
    }
    if let Some(entry) = first_existing(&[MOUNTED_DSH_ENTRY, IMAGE_DSH_ENTRY]) {
        return Some(DshSource::Mounted(entry));
    }
    if dsh_on_path() {
        return Some(DshSource::Path);
    }
    None
}

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

    let Some(source) = detect_source() else {
        return false;
    };
    let home = dsh_home();
    if !home.is_dir() {
        log::warn!(
            "host-agent: Dsh home {} is missing; skipping auto-registration",
            home.display()
        );
        return false;
    }

    let (command, args, source_label) = match &source {
        DshSource::Mounted(entry) => (
            node_binary(),
            vec![
                entry.to_string_lossy().into_owned(),
                "--profile".to_string(),
                "acp".to_string(),
            ],
            format!("mounted {}", entry.display()),
        ),
        DshSource::Path => (
            "dsh".to_string(),
            vec!["--profile".to_string(), "acp".to_string()],
            "PATH".to_string(),
        ),
    };
    // Marks the descriptor so the catalog ensure/probe path does not rewrite a
    // mounted command back to the catalog launcher.
    let mut env = HashMap::new();
    env.insert(HOST_MARKER_ENV.to_string(), "1".to_string());

    let mark_done = || {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(&marker, b"registered\n") {
            log::warn!("host-agent: could not write {}: {error}", marker.display());
        }
    };

    if let Ok(list) = agents.list_agents() {
        let existing = list.agents.iter().find(|agent| {
            agent.template == AgentTemplate::Dsh
                || (agent.command == command && agent.args == args)
        });
        if let Some(existing) = existing {
            let mut merged_env = existing.env.clone();
            merged_env.insert(HOST_MARKER_ENV.to_string(), "1".to_string());
            let needs_update = existing.template != AgentTemplate::Dsh
                || existing.command != command
                || existing.args != args
                || existing.env.get(HOST_MARKER_ENV) != Some(&"1".to_string());
            if needs_update {
                let migrated = UpsertAgentRequest {
                    id: Some(existing.id.clone()),
                    name: "Dsh".to_string(),
                    template: Some(AgentTemplate::Dsh),
                    command: command.clone(),
                    args: args.clone(),
                    env: merged_env,
                    set_default: false,
                };
                match agents.upsert_agent(migrated) {
                    Ok(_) => log::info!(
                        "host-agent: normalized existing Dsh agent ({source_label})"
                    ),
                    Err(error) => log::warn!("host-agent: could not normalize Dsh agent: {error}"),
                }
            }
            mark_done();
            return false;
        }
    }

    let request = UpsertAgentRequest {
        id: Some(HOST_AGENT_ID.to_string()),
        // Use the Dsh template so the catalog row shows the Dsh icon/name
        // instead of a second generic "Host Dsh" custom-agent row.
        name: "Dsh".to_string(),
        template: Some(AgentTemplate::Dsh),
        command,
        args,
        env,
        set_default: true,
    };
    match agents.upsert_agent(request) {
        Ok(agent) => {
            mark_done();
            log::info!(
                "host-agent: registered {} from {source_label} (Dsh home {})",
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
