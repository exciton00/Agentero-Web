//! Loopback Streamable HTTP MCP server for the headless Web host.
//!
//! Ported from the desktop Host (`src-tauri/src/integration/mcp/`) with the
//! Tauri seams replaced by the Web host's `EventBus` / `AppHandle` / settings
//! store:
//!
//! - the MCP listener is a separate axum server (`/mcp`) on
//!   `127.0.0.1:{settings.mcpPort}` by default; `AGENTERO_MCP_BIND` overrides
//!   the bind address (container / reverse-proxy deployments);
//! - `AGENTERO_MCP_ENABLED=1` force-starts it when `settings.mcpEnabled` is off;
//! - `mcp:status` events go through the process-wide [`EventBus`];
//! - the desktop job center, renderer wiki index and tunnel are absent; the
//!   tool surface adapts accordingly (see `tools.rs`).
//!
//! See `docs/backend/mcp.md`.

mod files;
mod icons;
mod layout;
mod notes;
mod paper;
mod resources;
mod server;
mod tools;

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agentero_core::app_handle::AppHandle;
use agentero_core::error::AppError;
use agentero_core::features::paper::capabilities::CapsCache;
use agentero_core::features::system::settings::{AppSettingsStore, DEFAULT_MCP_PORT};
use serde::Serialize;
use tokio::sync::oneshot;

use crate::events::EventBus;

/// Frontend event name (same payload as the desktop `mcp:status`).
pub const MCP_STATUS_EVENT: &str = "mcp:status";
/// Bind-address override, e.g. `0.0.0.0:8765`. Wins over settings (bind + port).
pub const ENV_MCP_BIND: &str = "AGENTERO_MCP_BIND";
/// `1` force-starts the listener even when `settings.mcpEnabled` is false.
pub const ENV_MCP_ENABLED: &str = "AGENTERO_MCP_ENABLED";
/// Extra allowed `Host` header values for reverse proxies (comma separated),
/// e.g. `mcp.example.com`. Loopback + bind host/port are always allowed.
pub const ENV_MCP_ALLOWED_HOSTS: &str = "AGENTERO_MCP_ALLOWED_HOSTS";

/// Status payload consumed by `src/lib/mcp/status.ts` (`McpStatus`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub enabled: bool,
    pub listening: bool,
    pub port: u16,
    /// MCP endpoint URL; only set while listening.
    pub url: Option<String>,
    pub last_error: Option<String>,
    pub vault_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VaultEntry {
    /// Absolute local vault path. Pass this as the `vault` tool argument.
    pub path: String,
    pub name: String,
    pub exists: bool,
    /// True for the vault the Web host currently serves.
    pub open: bool,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VaultListOut {
    pub vaults: Vec<VaultEntry>,
    pub open: Option<String>,
}

struct Inner {
    enabled: bool,
    listening: bool,
    port: u16,
    last_error: Option<String>,
    /// Effective address once bound (used by `status.url`).
    bound_addr: Option<SocketAddr>,
    vault_handle: Option<String>,
    /// Local vaults the user has opened, most-recent first. The server vault is
    /// always present; `mcp_set_known_vaults` replaces the rest.
    known_vaults: Vec<String>,
    parent_dir: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Handle of the spawned `server::serve` task; aborted (and awaited on the
    /// async stop path) so a port change never races the old listener.
    serve_task: Option<tokio::task::JoinHandle<()>>,
}

pub struct McpController {
    inner: Mutex<Inner>,
    running: AtomicBool,
    /// Serializes enable/disable/port mutations. Two overlapping
    /// `mcp_set_port` RPCs must not interleave their stop/bind steps: the loser
    /// of the race would otherwise overwrite the winner's `listening` /
    /// `last_error` state with a stale bind failure.
    lifecycle: tokio::sync::Mutex<()>,
    settings: Arc<AppSettingsStore>,
    events: EventBus,
    caps: CapsCache,
    /// Vault served by the Web host (`AGENTERO_WEB_VAULT`); always known/open.
    server_vault: PathBuf,
    /// Fixed bind address from `AGENTERO_MCP_BIND` (wins over settings).
    env_bind: Option<SocketAddr>,
    env_enabled: bool,
}

impl McpController {
    pub fn new(
        settings: Arc<AppSettingsStore>,
        events: EventBus,
        caps: CapsCache,
        server_vault: PathBuf,
    ) -> Self {
        let env_bind = match std::env::var(ENV_MCP_BIND) {
            Ok(raw) if !raw.trim().is_empty() => match parse_bind(&raw) {
                Some(addr) => Some(addr),
                None => {
                    log::warn!(
                        "{ENV_MCP_BIND}={raw:?} is not a valid host:port; using settings.mcpPort"
                    );
                    None
                }
            },
            _ => None,
        };
        let env_enabled = std::env::var(ENV_MCP_ENABLED)
            .map(|raw| matches!(raw.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        let snapshot = settings.get().ok().map(|result| result.settings);
        let settings_enabled = snapshot.as_ref().is_some_and(|settings| settings.mcp_enabled);
        let settings_port = snapshot
            .as_ref()
            .map(|settings| settings.mcp_port)
            .filter(|port| *port != 0)
            .unwrap_or(DEFAULT_MCP_PORT);
        let server_vault = agentero_core::fs::canonicalize_best_effort(&server_vault);
        let vault_handle = server_vault.to_string_lossy().into_owned();
        let port = env_bind.map(|addr| addr.port()).unwrap_or(settings_port);
        Self {
            inner: Mutex::new(Inner {
                enabled: env_enabled || settings_enabled,
                listening: false,
                port,
                last_error: None,
                bound_addr: None,
                vault_handle: Some(vault_handle.clone()),
                known_vaults: vec![vault_handle],
                parent_dir: "papers".into(),
                shutdown_tx: None,
                serve_task: None,
            }),
            running: AtomicBool::new(false),
            lifecycle: tokio::sync::Mutex::new(()),
            settings,
            events,
            caps,
            server_vault,
            env_bind,
            env_enabled,
        }
    }

    /// Test constructor: in-memory settings store, fresh event bus / caps, no
    /// host hooks and a placeholder vault (call `set_vault` to target a
    /// fixture). Production uses [`McpController::new`] via `AppState`.
    #[doc(hidden)]
    pub fn for_tests() -> Self {
        Self::new(
            Arc::new(AppSettingsStore::for_tests(
                agentero_core::features::system::settings::AppSettings::default(),
            )),
            EventBus::new(),
            CapsCache::new(),
            std::env::temp_dir().join("agentero-mcp-test-vault"),
        )
    }

    // ---- dependencies for the tool surface ---------------------------------

    pub fn caps(&self) -> &CapsCache {
        &self.caps
    }

    /// Fresh core host handle wired to this process's WebSocket event bus.
    /// `AppHandle` is not `Clone`; constructing one is cheap (one `Arc`), and
    /// every domain call that emits events builds its own.
    pub fn host_handle(&self) -> AppHandle {
        agentero_core::app_handle::AppHandle::with_hooks(Arc::new(
            crate::events::WebHostHooks {
                bus: self.events.clone(),
            },
        ))
    }

    /// `paper_text_get` opt-in, read live from the settings store.
    pub fn expose_paper_text(&self) -> bool {
        self.settings
            .get()
            .map(|result| result.settings.mcp_expose_paper_text)
            .unwrap_or(false)
    }

    /// Translation base URL for `import_id` (empty → engine default).
    pub fn translator_url(&self) -> Option<String> {
        self.settings
            .get()
            .ok()
            .map(|result| result.settings.translator_base_url.trim().to_string())
            .filter(|url| !url.is_empty())
    }

    /// `paperNoteMode` for imported NOTES.md shells.
    pub fn paper_note_mode(&self) -> String {
        self.settings
            .get()
            .map(|result| result.settings.paper_note_mode)
            .unwrap_or_else(|_| "standard".into())
    }

    // ---- lifecycle --------------------------------------------------------

    pub fn status(&self) -> McpStatus {
        let guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        Self::status_from(&guard)
    }

    fn status_from(inner: &Inner) -> McpStatus {
        let url = if inner.listening {
            let addr = inner
                .bound_addr
                .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], inner.port)));
            Some(format!(
                "http://{}:{}/mcp",
                display_host(addr.ip()),
                addr.port()
            ))
        } else {
            None
        };
        McpStatus {
            enabled: inner.enabled,
            listening: inner.listening,
            port: inner.port,
            url,
            last_error: inner.last_error.clone(),
            vault_path: inner.vault_handle.clone(),
        }
    }

    pub fn port(&self) -> u16 {
        self.inner
            .lock()
            .map(|inner| inner.port)
            .unwrap_or(DEFAULT_MCP_PORT)
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.lock().map(|inner| inner.enabled).unwrap_or(false)
    }

    /// True when `AGENTERO_MCP_BIND` pins the bind address (and port).
    pub fn port_fixed_by_env(&self) -> bool {
        self.env_bind.is_some()
    }

    /// Address the next `start_server` binds: `AGENTERO_MCP_BIND` when set,
    /// otherwise loopback + the configured port.
    fn bind_addr(&self) -> SocketAddr {
        if let Some(addr) = self.env_bind {
            return addr;
        }
        let port = self.port();
        SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port)
    }

    /// Human-readable bind target for startup logs / bind errors.
    pub fn bind_display(&self) -> String {
        self.bind_addr().to_string()
    }

    /// Start when settings/env ask for it. Called by `main` after `AppState`
    /// is built; a bind failure is reported through `last_error` (not fatal).
    pub async fn start_if_configured(self: &Arc<Self>) {
        let _lifecycle = self.lifecycle.lock().await;
        let desired = self.is_enabled();
        let bind = self.bind_addr();
        if desired {
            if let Err(error) = self.start_server().await {
                log::warn!("mcp: cannot start on {bind}: {error}");
                if let Ok(mut inner) = self.inner.lock() {
                    inner.enabled = true;
                    inner.listening = false;
                    inner.last_error = Some(error.to_string());
                }
                self.running.store(false, Ordering::SeqCst);
            } else if let Some(url) = self.status().url {
                log::info!("mcp listening on {url}");
            }
        }
        self.emit_status();
    }

    pub async fn set_enabled(self: &Arc<Self>, enabled: bool) -> McpStatus {
        let _lifecycle = self.lifecycle.lock().await;
        if !enabled {
            self.stop_listener().await;
            if let Ok(mut inner) = self.inner.lock() {
                inner.enabled = false;
                inner.last_error = None;
            }
            self.emit_status();
            return self.status();
        }
        {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            inner.enabled = true;
            inner.last_error = None;
            if inner.listening {
                return Self::status_from(&inner);
            }
        }
        if let Err(error) = self.start_server().await {
            if let Ok(mut inner) = self.inner.lock() {
                inner.enabled = true;
                inner.listening = false;
                inner.last_error = Some(error.to_string());
            }
            self.running.store(false, Ordering::SeqCst);
        }
        self.emit_status();
        self.status()
    }

    pub async fn set_port(self: &Arc<Self>, port: u16) -> McpStatus {
        let _lifecycle = self.lifecycle.lock().await;
        if port == 0 {
            if let Ok(mut inner) = self.inner.lock() {
                inner.last_error = Some("MCP port must be between 1 and 65535".into());
            }
            self.emit_status();
            return self.status();
        }
        if self.port_fixed_by_env() {
            if let Ok(mut inner) = self.inner.lock() {
                inner.last_error =
                    Some(format!("MCP port is fixed by {ENV_MCP_BIND} in this deployment"));
            }
            self.emit_status();
            return self.status();
        }
        let enabled = self.is_enabled();
        if enabled {
            // Await the old serve task so its listener is actually closed before
            // `start_server` binds: an immediate retry/revert must not see the
            // previous port as still in use.
            self.stop_listener().await;
        }
        if let Ok(mut inner) = self.inner.lock() {
            inner.port = port;
            inner.last_error = None;
        }
        if enabled {
            if let Err(error) = self.start_server().await {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.last_error = Some(error.to_string());
                    inner.listening = false;
                }
            }
        }
        self.emit_status();
        self.status()
    }

    /// Stop the listener and mark the feature disabled.
    pub fn stop(&self) {
        self.stop_server();
        if let Ok(mut inner) = self.inner.lock() {
            inner.enabled = false;
        }
        self.emit_status();
    }

    fn stop_server(&self) {
        if let Some(task) = self.take_listener() {
            task.abort();
        }
        if let Ok(mut inner) = self.inner.lock() {
            inner.listening = false;
            inner.bound_addr = None;
        }
        self.running.store(false, Ordering::SeqCst);
    }

    /// Async stop: abort the serve task and **await** it, so the listening
    /// socket is closed when this returns. This is what makes
    /// `mcp_set_port` / `mcp_set_enabled(false)` immediately re-bindable.
    async fn stop_listener(&self) {
        if let Some(task) = self.take_listener() {
            task.abort();
            let _ = task.await;
        }
        self.running.store(false, Ordering::SeqCst);
    }

    /// Tear down the current listener state and hand the serve task back to the
    /// caller (which decides whether to abort / await it).
    fn take_listener(&self) -> Option<tokio::task::JoinHandle<()>> {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(tx) = inner.shutdown_tx.take() {
            let _ = tx.send(());
        }
        inner.listening = false;
        inner.bound_addr = None;
        inner.serve_task.take()
    }

    async fn start_server(self: &Arc<Self>) -> Result<(), AppError> {
        let addr = self.bind_addr();
        self.stop_listener().await;

        let listener = bind_listener(addr).await?;
        let bound = listener.local_addr().unwrap_or(addr);
        if !bound.ip().is_loopback() {
            log::warn!(
                "mcp: binding {bound} — MCP has no authentication; expose it only behind an authenticating reverse proxy"
            );
        }
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            inner.shutdown_tx = Some(shutdown_tx);
            inner.listening = true;
            inner.bound_addr = Some(bound);
            inner.port = bound.port();
            inner.last_error = None;
        }
        self.running.store(true, Ordering::SeqCst);

        let ctrl = Arc::clone(self);
        let serve_ctrl = Arc::clone(self);
        let serve_task = tokio::spawn(async move {
            if let Err(error) = server::serve(listener, shutdown_rx, serve_ctrl).await {
                if let Ok(mut inner) = ctrl.inner.lock() {
                    inner.listening = false;
                    inner.bound_addr = None;
                    inner.last_error = Some(error.to_string());
                }
                ctrl.running.store(false, Ordering::SeqCst);
                ctrl.emit_status();
            }
        });
        if let Ok(mut inner) = self.inner.lock() {
            inner.serve_task = Some(serve_task);
        }
        Ok(())
    }

    fn emit_status(&self) {
        self.events.emit(MCP_STATUS_EVENT, &self.status());
    }

    // ---- vault scoping ----------------------------------------------------

    pub fn set_vault(&self, vault_path: Option<String>) {
        let server_vault = self.server_vault.to_string_lossy().into_owned();
        let next = vault_path
            .as_deref()
            .and_then(normalize_local_vault)
            .map(|path| path.to_string_lossy().into_owned())
            // The Web host always serves one configured vault: `None` (or an
            // invalid path) falls back to it instead of clearing the scope.
            .unwrap_or(server_vault.clone());
        if let Ok(mut inner) = self.inner.lock() {
            remember_vault(&mut inner.known_vaults, &next);
            if !inner
                .known_vaults
                .iter()
                .any(|path| canonical_vault_key(path) == canonical_vault_key(&server_vault))
            {
                remember_vault(&mut inner.known_vaults, &server_vault);
            }
            inner.vault_handle = Some(next);
        }
        self.emit_status();
    }

    /// Replace the known-vault list with the renderer's recent local vaults.
    /// Unknown paths are rejected by [`resolve_vault`](Self::resolve_vault), so
    /// external clients can only target vaults the frontend synced.
    pub fn set_known_vaults(&self, vaults: Vec<String>) {
        let mut normalized = Vec::new();
        for raw in vaults.iter().rev() {
            if let Some(path) = normalize_local_vault(raw) {
                remember_vault(&mut normalized, &path.to_string_lossy());
            }
        }
        let server_vault = self.server_vault.to_string_lossy().into_owned();
        remember_vault(&mut normalized, &server_vault);
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(open) = inner.vault_handle.clone() {
                remember_vault(&mut normalized, &open);
            }
            inner.known_vaults = normalized;
        }
    }

    pub fn set_parent_dir(&self, parent_dir: String) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.parent_dir = parent_dir;
        }
    }

    pub fn parent_dir(&self) -> String {
        self.inner
            .lock()
            .map(|inner| inner.parent_dir.clone())
            .unwrap_or_else(|_| "papers".into())
    }

    /// Vault served when a tool omits its `vault` argument.
    pub fn local_vault(&self) -> Result<PathBuf, AppError> {
        let handle = {
            let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            inner.vault_handle.clone()
        };
        let Some(handle) = handle else {
            return Err(AppError::message(
                "No vault configured — set AGENTERO_WEB_VAULT or open a vault first",
            ));
        };
        if handle.starts_with("remote:") {
            return Err(AppError::message("MCP requires a local vault"));
        }
        agentero_core::fs::resolve_vault(&handle)
    }

    /// All local vaults the user has opened, newest first. The server vault is
    /// always present and marked `open`.
    pub fn vault_list(&self) -> VaultListOut {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let open = inner.vault_handle.clone();
        let open_key = open.as_deref().map(canonical_vault_key);
        let mut paths = Vec::new();
        for path in inner.known_vaults.iter().rev() {
            remember_vault(&mut paths, path);
        }
        let server_vault = self.server_vault.to_string_lossy().into_owned();
        remember_vault(&mut paths, &server_vault);
        if let Some(path) = &open {
            remember_vault(&mut paths, path);
        }
        let vaults = paths
            .into_iter()
            .map(|path| {
                let exists = Path::new(&path).is_dir();
                let name = Path::new(&path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| path.clone());
                VaultEntry {
                    open: open_key.as_ref() == Some(&canonical_vault_key(&path)),
                    path,
                    name,
                    exists,
                }
            })
            .collect();
        VaultListOut { vaults, open }
    }

    /// Resolve a tool's optional `vault` argument. Only the open vault or a
    /// frontend-synced known vault is accepted, so an external client cannot
    /// point tools at arbitrary filesystem roots.
    pub fn resolve_vault(&self, requested: Option<&str>) -> Result<PathBuf, AppError> {
        let Some(raw) = requested.map(str::trim).filter(|raw| !raw.is_empty()) else {
            return self.local_vault();
        };
        let Some(path) = normalize_local_vault(raw) else {
            return Err(AppError::message(
                "vault must be an absolute local vault path from vault_list",
            ));
        };
        let key = canonical_vault_key(&path.to_string_lossy());
        let known = {
            let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            let open = inner.vault_handle.as_deref().map(canonical_vault_key);
            open.as_ref() == Some(&key)
                || inner
                    .known_vaults
                    .iter()
                    .any(|known| canonical_vault_key(known) == key)
                || canonical_vault_key(&self.server_vault.to_string_lossy()) == key
        };
        if !known {
            return Err(AppError::message(format!(
                "vault is not open or known: {} — call vault_list first",
                path.display()
            )));
        }
        agentero_core::fs::resolve_vault(&path.to_string_lossy())
    }
}

/// Bind the MCP listener with `SO_REUSEADDR` and a short retry window.
///
/// Closing a listening socket that still has accepted MCP/SSE connections can
/// keep the port unusable for a moment; `SO_REUSEADDR` plus a few retries makes
/// an immediate port revert (or restart after a failed bind) succeed instead of
/// surfacing a spurious `Address already in use`.
async fn bind_listener(addr: SocketAddr) -> Result<tokio::net::TcpListener, AppError> {
    const ATTEMPTS: usize = 5;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
    let mut last_error = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(RETRY_DELAY).await;
        }
        match bind_with_reuseaddr(addr) {
            Ok(listener) => return Ok(listener),
            Err(error) => last_error = Some(error),
        }
    }
    Err(AppError::message(format!(
        "Failed to bind {addr}: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "unknown error".into())
    )))
}

fn bind_with_reuseaddr(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    use tokio::net::TcpSocket;
    let socket = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(1024)
}

/// Parse `AGENTERO_MCP_BIND` (`host:port` / `[::1]:port`).
fn parse_bind(raw: &str) -> Option<SocketAddr> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<SocketAddr>().ok()
}

/// URL host for a bound address: loopback / wildcard → `127.0.0.1` (wildcard
/// is not connectable); concrete hosts are used as-is.
fn display_host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) if v4.is_unspecified() || v4.is_loopback() => "127.0.0.1".into(),
        IpAddr::V6(v6) if v6.is_unspecified() || v6.is_loopback() => "127.0.0.1".into(),
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

fn normalize_local_vault(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with("remote:") {
        return None;
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return None;
    }
    Some(agentero_core::fs::canonicalize_best_effort(&path))
}

fn canonical_vault_key(path: &str) -> String {
    normalize_local_vault(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| agentero_core::fs::normalize_rel_separators(path.trim()))
}

fn remember_vault(list: &mut Vec<String>, path: &str) {
    let Some(path) = normalize_local_vault(path) else {
        return;
    };
    let path = path.to_string_lossy().into_owned();
    let key = canonical_vault_key(&path);
    list.retain(|existing| canonical_vault_key(existing) != key);
    list.insert(0, path);
    list.truncate(32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentero_core::features::system::settings::AppSettings;

    fn controller(vault: &Path) -> Arc<McpController> {
        Arc::new(McpController::new(
            Arc::new(AppSettingsStore::for_tests(AppSettings::default())),
            EventBus::new(),
            CapsCache::new(),
            vault.to_path_buf(),
        ))
    }

    #[test]
    fn status_payload_matches_frontend_shape_and_is_null_url_when_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = controller(dir.path());
        let value = serde_json::to_value(ctrl.status()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "enabled": false,
                "listening": false,
                "port": DEFAULT_MCP_PORT,
                "url": null,
                "lastError": null,
                "vaultPath": dir.path().canonicalize().unwrap().to_string_lossy(),
            })
        );
        assert_eq!(
            ctrl.status().vault_path.as_deref(),
            Some(dir.path().canonicalize().unwrap().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn vault_list_defaults_to_the_server_vault_and_rejects_unknown_paths() {
        let dir = tempfile::tempdir().unwrap();
        let unknown = tempfile::tempdir().unwrap();
        let ctrl = controller(dir.path());
        let key = |path: &Path| {
            agentero_core::fs::canonicalize_best_effort(path)
                .to_string_lossy()
                .into_owned()
        };
        let listed = ctrl.vault_list();
        assert_eq!(listed.open.as_deref(), Some(key(dir.path()).as_str()));
        assert_eq!(listed.vaults.len(), 1);
        assert!(listed.vaults[0].open);
        assert_eq!(listed.vaults[0].path, key(dir.path()));
        assert_eq!(
            ctrl.resolve_vault(Some(&key(dir.path()))).unwrap(),
            dir.path().canonicalize().unwrap()
        );
        assert!(ctrl.resolve_vault(Some(&key(unknown.path()))).is_err());
        assert!(ctrl.resolve_vault(Some("relative/vault")).is_err());
        assert!(ctrl.resolve_vault(Some("remote:session-1")).is_err());
        assert_eq!(
            ctrl.resolve_vault(None).unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn known_vaults_are_advertised_but_only_server_vault_is_open() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let ctrl = controller(first.path());
        let key = |path: &Path| {
            agentero_core::fs::canonicalize_best_effort(path)
                .to_string_lossy()
                .into_owned()
        };
        ctrl.set_known_vaults(vec![key(second.path()), "remote:x".into(), "../rel".into()]);
        let listed = ctrl.vault_list();
        let paths: Vec<&str> = listed.vaults.iter().map(|v| v.path.as_str()).collect();
        assert!(paths.contains(&key(first.path()).as_str()));
        assert!(paths.contains(&key(second.path()).as_str()));
        assert_eq!(listed.vaults.iter().filter(|v| v.open).count(), 1);
        assert!(ctrl.resolve_vault(Some(&key(second.path()))).is_ok());
        // The server vault is always targetable, even for another open vault.
        ctrl.set_vault(Some(key(second.path())));
        assert!(ctrl.resolve_vault(Some(&key(first.path()))).is_ok());
        let listed = ctrl.vault_list();
        assert_eq!(
            listed.open.as_deref(),
            Some(key(second.path()).as_str()),
            "mcp_set_vault switches the tool scope"
        );
    }

    #[test]
    fn parent_dir_defaults_to_papers_and_updates() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = controller(dir.path());
        assert_eq!(ctrl.parent_dir(), "papers");
        ctrl.set_parent_dir("papers/nlp".into());
        assert_eq!(ctrl.parent_dir(), "papers/nlp");
    }

    #[test]
    fn expose_paper_text_reads_the_live_settings_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(AppSettingsStore::for_tests(AppSettings::default()));
        let ctrl = Arc::new(McpController::new(
            Arc::clone(&store),
            EventBus::new(),
            CapsCache::new(),
            dir.path().to_path_buf(),
        ));
        assert!(!ctrl.expose_paper_text(), "opt-in is off by default");
        let mut settings = store.get().unwrap().settings;
        settings.mcp_expose_paper_text = true;
        store.set(settings).unwrap();
        assert!(ctrl.expose_paper_text(), "flipping settings is live");
    }

    #[tokio::test]
    async fn enable_listens_disable_stops_and_emits_status() {
        let dir = tempfile::tempdir().unwrap();
        let events = EventBus::new();
        let mut frames = events.subscribe();
        let ctrl = Arc::new(McpController::new(
            Arc::new(AppSettingsStore::for_tests(AppSettings::default())),
            events.clone(),
            CapsCache::new(),
            dir.path().to_path_buf(),
        ));
        // Reserve an ephemeral port, then release it for the MCP listener.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let status = ctrl.set_port(port).await;
        assert!(!status.listening);
        assert_eq!(status.port, port);

        let status = ctrl.set_enabled(true).await;
        assert!(status.listening, "{status:?}");
        assert!(status.url.as_deref().unwrap().ends_with("/mcp"));
        assert!(status.url.as_deref().unwrap().contains(&format!("127.0.0.1:{port}")));

        // `mcp:status` must reach the WebSocket bus on every transition.
        let frame = frames.recv().await.expect("status event");
        assert_eq!(frame.event, MCP_STATUS_EVENT);
        assert_eq!(frame.payload["listening"], false, "port change is reported");
        let mut saw_listening = false;
        while let Ok(frame) = frames.try_recv() {
            assert_eq!(frame.event, MCP_STATUS_EVENT);
            if frame.payload["listening"] == true {
                saw_listening = true;
            }
        }
        assert!(saw_listening, "listening status event emitted");

        let status = ctrl.set_enabled(false).await;
        assert!(!status.listening);
        assert!(!status.enabled);
        assert_eq!(status.url, None);
    }

    /// Regression for the frontend report: a failed port change (target port
    /// occupied) followed by an immediate revert to the previous port must bind
    /// on the first try, not report the old port as still in use.
    #[tokio::test]
    async fn failed_port_change_can_immediately_revert() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = controller(dir.path());
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port_a = probe.local_addr().unwrap().port();
        drop(probe);
        ctrl.set_port(port_a).await;
        let status = ctrl.set_enabled(true).await;
        assert!(status.listening, "{status:?}");

        // Occupy another port so changing to it fails at bind time.
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port_b = occupied.local_addr().unwrap().port();
        let status = ctrl.set_port(port_b).await;
        assert!(!status.listening, "{status:?}");
        assert!(
            status
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("Failed to bind"),
            "{status:?}"
        );

        // The old listener must be fully gone: revert binds immediately.
        let status = ctrl.set_port(port_a).await;
        assert!(
            status.listening,
            "revert to the previous port must bind immediately: {status:?}"
        );
        assert_eq!(status.port, port_a);
        ctrl.set_enabled(false).await;
    }

    /// Toggling off/on must not race the previous listener either.
    #[tokio::test]
    async fn disable_then_enable_rebinds_the_same_port_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = controller(dir.path());
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        ctrl.set_port(port).await;
        assert!(ctrl.set_enabled(true).await.listening);
        assert!(!ctrl.set_enabled(false).await.listening);
        let status = ctrl.set_enabled(true).await;
        assert!(
            status.listening,
            "re-enable must rebind the same port immediately: {status:?}"
        );
        ctrl.set_enabled(false).await;
    }

    /// Overlapping port RPCs must serialize. Frontend observed a stale
    /// `lastError` + `listening=false` when a second change was submitted while
    /// the first (~250ms bind retry window) was still in flight: the failing
    /// call must not overwrite the successful one's state.
    #[tokio::test]
    async fn concurrent_port_changes_serialize_without_stale_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = controller(dir.path());
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port_a = probe.local_addr().unwrap().port();
        drop(probe);
        ctrl.set_port(port_a).await;
        assert!(ctrl.set_enabled(true).await.listening);

        // A port that is guaranteed to fail the bind.
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port_b = occupied.local_addr().unwrap().port();

        let failing = {
            let ctrl = ctrl.clone();
            tokio::spawn(async move { ctrl.set_port(port_b).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let revert = {
            let ctrl = ctrl.clone();
            tokio::spawn(async move { ctrl.set_port(port_a).await })
        };
        let failing_status = failing.await.unwrap();
        let revert_status = revert.await.unwrap();
        assert!(!failing_status.listening, "{failing_status:?}");
        assert!(revert_status.listening, "{revert_status:?}");

        let final_status = ctrl.status();
        assert!(
            final_status.listening,
            "final state must be the reverted listener: {final_status:?}"
        );
        assert_eq!(final_status.port, port_a);
        assert_eq!(
            final_status.last_error, None,
            "a superseded bind failure must not leak into the final state"
        );
        ctrl.set_enabled(false).await;
    }

    #[tokio::test]
    async fn invalid_port_reports_business_error_without_changing_the_listener() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = controller(dir.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        ctrl.set_port(port).await;
        let before = ctrl.status();
        let status = ctrl.set_port(0).await;
        assert_eq!(status.port, before.port);
        assert_eq!(
            status.last_error.as_deref(),
            Some("MCP port must be between 1 and 65535")
        );
    }
}
