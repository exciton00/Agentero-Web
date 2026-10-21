//! Process configuration from environment variables.
//!
//! All paths are absolute or expanded from `$HOME`; the server is single-user
//! and assumes the reverse proxy owns authentication.

use std::net::SocketAddr;
use std::path::PathBuf;

/// Environment variable holding the listen address (`host:port`).
pub const ENV_ADDR: &str = "AGENTERO_WEB_ADDR";
/// Environment variable holding the default Vault root.
pub const ENV_VAULT: &str = "AGENTERO_WEB_VAULT";
/// Environment variable holding the built SPA directory.
pub const ENV_DIST: &str = "AGENTERO_WEB_DIST";

/// Default listen address (loopback; put a reverse proxy in front for remote).
pub const DEFAULT_ADDR: &str = "127.0.0.1:8787";
/// Relative path of the built frontend inside a deployment image.
pub const DEFAULT_DIST: &str = "dist";

#[derive(Debug, Clone)]
pub struct Config {
    pub addr: SocketAddr,
    /// Default Vault root; commands still receive an explicit `vaultPath`, this
    /// is only used for the `/api/file` endpoint and first-run defaults.
    pub vault_root: PathBuf,
    /// Built SPA directory; `None` when it does not exist (API-only mode).
    pub dist_dir: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let addr = std::env::var(ENV_ADDR)
            .unwrap_or_else(|_| DEFAULT_ADDR.to_string())
            .parse::<SocketAddr>()
            .map_err(|error| format!("invalid {ENV_ADDR}: {error}"))?;

        let vault_root = match std::env::var(ENV_VAULT) {
            Ok(value) if !value.trim().is_empty() => expand_home(value.trim()),
            _ => default_vault_root(),
        };

        let dist_dir = std::env::var(ENV_DIST)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| expand_home(value.trim()))
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(DEFAULT_DIST)
            });

        Ok(Self {
            addr,
            vault_root,
            dist_dir: dist_dir.is_dir().then_some(dist_dir),
        })
    }
}

/// `~/Agentero` by default: a single visible directory that can be mounted as
/// a Docker volume and read by external tools.
fn default_vault_root() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join("Agentero")
}

fn expand_home(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(rest);
    }
    PathBuf::from(value)
}
