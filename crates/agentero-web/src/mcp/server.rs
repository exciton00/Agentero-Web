//! Streamable HTTP MCP listener (loopback by default, `/mcp` root path).
//!
//! A dedicated axum server on its own port; the main Web host router is not
//! involved. See `docs/backend/mcp.md`.

use std::net::SocketAddr;
use std::sync::Arc;

use agentero_core::error::AppError;
use axum::Router;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::tools::AgenteroMcp;
use super::{McpController, ENV_MCP_ALLOWED_HOSTS};

pub async fn serve(
    listener: TcpListener,
    shutdown_rx: oneshot::Receiver<()>,
    ctrl: Arc<McpController>,
) -> Result<(), AppError> {
    let bound = listener.local_addr().ok();
    let mut config = StreamableHttpServerConfig::default();
    config.allowed_hosts = allowed_hosts(bound);
    let mcp_service = StreamableHttpService::new(
        {
            let ctrl = Arc::clone(&ctrl);
            move || Ok(AgenteroMcp::new(Arc::clone(&ctrl)))
        },
        LocalSessionManager::default().into(),
        config,
    );
    let app = Router::new().nest_service("/mcp", mcp_service);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await
        .map_err(|error| AppError::message(format!("mcp server: {error}")))
}

/// `Host` header values the Streamable HTTP transport accepts.
///
/// rmcp rejects requests whose `Host` is not listed (DNS-rebinding guard), so
/// the listener address, its loopback spellings and any reverse-proxy host from
/// `AGENTERO_MCP_ALLOWED_HOSTS` must all be present. An entry without a port
/// matches every port.
fn allowed_hosts(bound: Option<SocketAddr>) -> Vec<String> {
    let mut hosts = vec![
        "127.0.0.1".to_string(),
        "localhost".to_string(),
        "[::1]".to_string(),
    ];
    if let Some(addr) = bound {
        let port = addr.port();
        let ip = addr.ip();
        hosts.push(ip.to_string());
        hosts.push(format!("{ip}:{port}"));
        if ip.is_loopback() || ip.is_unspecified() {
            hosts.push("localhost".to_string());
            hosts.push(format!("localhost:{port}"));
            hosts.push("127.0.0.1".to_string());
            hosts.push(format!("127.0.0.1:{port}"));
        }
        if ip.is_ipv6() {
            hosts.push(format!("[{ip}]"));
            hosts.push(format!("[{ip}]:{port}"));
        }
    }
    if let Ok(extra) = std::env::var(ENV_MCP_ALLOWED_HOSTS) {
        for host in extra.split(',') {
            let host = host.trim();
            if !host.is_empty() {
                hosts.push(host.to_string());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|host| seen.insert(host.to_ascii_lowercase()));
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_hosts_cover_loopback_and_bind_address() {
        let hosts = allowed_hosts(Some("127.0.0.1:8795".parse().unwrap()));
        for expected in [
            "127.0.0.1",
            "localhost",
            "127.0.0.1:8795",
            "localhost:8795",
        ] {
            assert!(hosts.iter().any(|host| host == expected), "{hosts:?}");
        }
    }

    #[test]
    fn allowed_hosts_cover_wildcard_bind_and_env_extras() {
        // Container bind: clients reach it through the published host/port or a
        // reverse proxy; env extras cover that host name.
        let hosts = allowed_hosts(Some("0.0.0.0:8765".parse().unwrap()));
        assert!(hosts.iter().any(|host| host == "127.0.0.1:8765"));
        assert!(hosts.iter().any(|host| host == "localhost:8765"));
        assert!(hosts.iter().any(|host| host == "0.0.0.0:8765"));
    }
}
